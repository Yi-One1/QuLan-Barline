#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""G9 组①扩展：受控可行性实验（零注入优先，分级执行）。

回答两个问题（结论一律由真机实验产出，本程序不伪造任何观察）：
  Q-a  能否安全定位并操作 XAML/Composition 视觉树里的三类元素，
       尤其是「有没有可写的前景 / 颜色属性入口」。
  Q-b  注入模块能否在不强制重启 Explorer 的前提下完整卸载
       （Explorer 存活、窗口树无残留、无持久状态）。

分级实验（精确 spec，不扩大范围）：
  阶段 0（零注入，默认执行）
    外部进程只读探测：UI Automation COM 客户端能力探测 + Win32 只读窗口树
    枚举 + 环境 fail-safe 判定 + 窗口树快照哈希。不注入、不写任何东西。
    UIA 完整自动化树遍历在「零第三方依赖 + 禁 Add-Type」约束下不执行，
    如实标注 UIA 路径未完成与原因，绝不伪造任何 UIA 节点命中结果。
  阶段 1（只读注入，默认关闭）
    仅当显式传入 --allow-inject 且由主理人真机执行时运行。加载自建的最小
    只读探针 DLL，DllMain 什么都不做，导出函数只做只读枚举并回写结果；
    不装任何 hook、不替换窗口过程、不写颜色 / 注册表 / 合成属性、不强杀进程。
    因无窗口过程指向该 DLL，卸载必须 FreeLibrary + VirtualFreeEx + 完整性校验。

看门狗（独立于实验的远程线程，主进程监督）
  默认硬超时 15s（--timeout 可配）；超时 / 实验进程异常退出 / 崩溃都要走卸载
  流程；每一步写 g9.experiment.* 结构化日志（阶段、动作、耗时、HRESULT、
  卸载结果、Explorer 存活校验、窗口树快照哈希）。任何失败都不得自动重启
  Explorer、不得自动重试注入，只记录并退出非零码。

--self-test（验收口径）
  纯逻辑自检：分类真值表 + fail-safe 环境真值表 + 看门狗状态机四条路径
  （正常 / 超时 / 异常 / FailSafe 不匹配，用可注入 Fake）+ 卸载完整性
  （每笔 install 都有对应 uninstall）+ 红线源码静态断言 + 反向验证。
  全程零系统 API / 零注入 / 零注册表写入 / 零颜色写入；仅用标准库读源码
  文本做静态扫描。文件末尾有「零系统 API 触碰」元断言可证明这一点。

设计纪律（对应 G9 与 G8 血泪教训）
  1. 所有 Win32 / COM 绑定都是惰性的，模块导入时不加载任何系统 DLL；
     self-test 分支不调用它们，因此可在无任务栏 / 无 Explorer 的环境跑。
  2. 分类、环境判定、窗口树哈希、看门狗状态机、卸载记账都是纯逻辑，
     与真实系统 API 解耦，因而能被 Fake 完整测试。
  3. 真实阶段 1 与 Fake 测试共用同一个 WatchdogFSM.run，测试锁得住接线
     （避免「测试全绿但真机路径没走状态机」）。
  4. 红线禁用 API 名一律「分片拼接」定义，使本文件源码文本里不出现任何
     完整的禁用 API 名连续字面量 —— 这样源码静态断言可以安全扫描本文件
     自身而不自我命中；运行时得到的仍是完整 token。
  5. 严禁把推断写成观察、严禁伪造真机注入结果。所有未执行 / 未完成的
     路径都显式标注 completed=false 与原因。

用法：
    python scripts/g9_inject_probe.py --self-test
    python scripts/g9_inject_probe.py                      # 阶段 0 零注入只读
    python scripts/g9_inject_probe.py --timeout 20
    python scripts/g9_inject_probe.py --allow-inject       # 阶段 1（主理人真机执行）
    python scripts/g9_inject_probe.py --allow-inject --timeout 30 \
        --log-file <path> --out-json <path> --env-json <path>

零第三方依赖；仅用 Python 标准库（ctypes / json / hashlib / argparse ...）。
本机禁 Add-Type / reg.exe / cmd.exe，注册表只读走 Win32 API。
"""

from __future__ import annotations

import argparse
import ctypes
import glob
import hashlib
import json
import math
import os
import sys
import time
from collections import Counter
from ctypes import wintypes
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Sequence, Tuple

# --------------------------------------------------------------------------- #
# 常量与路径（纯计算，不触碰系统）
# --------------------------------------------------------------------------- #

EXPERIMENT_ID = "g9.inject-feasibility/1.0"
EVENT_PREFIX = "g9.experiment"
DEFAULT_TIMEOUT_S = 15.0
STILL_ACTIVE = 259
WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 0x102
MEM_RELEASE = 0x8000
TH32CS_SNAPMODULE = 0x00000008
TH32CS_SNAPMODULE32 = 0x00000010
CLSCTX_INPROC_SERVER = 1
COINIT_APARTMENTTHREADED = 0x2
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_CREATE_THREAD = 0x0002
PROCESS_VM_OPERATION = 0x0008
PROCESS_VM_WRITE = 0x0020
PROCESS_VM_READ = 0x0010
MEM_COMMIT = 0x00001000
MEM_RESERVE = 0x00002000
PAGE_READWRITE = 0x04

_SCRIPT_PATH = os.path.abspath(__file__)
_SCRIPT_DIR = os.path.dirname(_SCRIPT_PATH)
_PROBE_DIR = os.path.join(_SCRIPT_DIR, "g9_inject_probe")
_DLL_PATH = os.path.join(_PROBE_DIR, "g9_readonly_probe.dll")
_PROBE_C_PATH = os.path.join(_PROBE_DIR, "probe.c")
_RESULT_PATH = os.path.join(_PROBE_DIR, "inject_result.txt")
_BUILD_RESULT_PATH = os.path.join(_PROBE_DIR, "build-result.json")

# 红线规则表最小条数（P1：锁绝对内容，防止「把 36 条削到 1 条仍全绿」）。
MIN_REDLINE_RULE_COUNT = 36

# 关键元开关：任何真实系统 API 绑定 / 调用都会把它置为 True。
# self_test() 结束时断言它仍为 False，从而「证明」自测零系统 API 触碰。
_SYSTEM_API_TOUCHED = False


def _touch_system_api() -> None:
    """标记「已触碰真实系统 API」。仅惰性系统层调用，self-test 绝不触发。"""
    global _SYSTEM_API_TOUCHED
    _SYSTEM_API_TOUCHED = True


# --------------------------------------------------------------------------- #
# 纯逻辑：分类真值表（复用组① 探针思路，独立实现，不 import 它）
# --------------------------------------------------------------------------- #

CLASSIC_CLOCK_MARKERS = (
    "TrayClockWClass", "ClockButton", "TrayNotifyWnd",
    "SysPager", "ToolbarWindow32", "TrayClockWClass2",
)
XAML_MARKERS = (
    "Windows.UI.Composition", "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow", "TopLevelWindowForOverflowXamlIsland",
    "Xaml_WindowedPopupClass", "DesktopWindowXamlSource",
)
TASKLIST_MARKERS = ("MSTaskListWClass", "MSTaskSwWClass", "ReBarWindow32")

SAMPLE_CLASSIC = ["Shell_TrayWnd", "TrayNotifyWnd", "TrayClockWClass",
                  "SysPager", "ToolbarWindow32"]
SAMPLE_XAML = ["Shell_TrayWnd", "Windows.UI.Composition.DesktopWindowContentBridge",
               "Windows.UI.Core.CoreWindow", "XamlExplorerHostIslandWindow"]
SAMPLE_MIXED = ["Shell_TrayWnd", "TrayNotifyWnd", "TrayClockWClass",
                "Windows.UI.Composition.DesktopWindowContentBridge",
                "MSTaskListWClass"]
SAMPLE_EMPTY = ["Shell_TrayWnd"]


def classify_tree(classes: Sequence[str]) -> Dict[str, Any]:
    """纯函数：给定窗口类名列表，判定任务栏绘制面形态。

    verdict:
      classic_hookable —— 有经典时钟 / 托盘窗口，用户态子类化理论上可有绘制面
      mixed             —— 经典窗口与 XAML 岛并存
      xaml_only         —— 只有 XAML / Composition 视觉树，无经典绘制面
      unknown           —— 信息不足
    """
    cs = [str(c) for c in classes]
    clock = sorted({c for c in cs
                    if any(m.lower() in c.lower() for m in CLASSIC_CLOCK_MARKERS)})
    xaml = sorted({c for c in cs
                   if any(m.lower() in c.lower() for m in XAML_MARKERS)})
    tasklist = sorted({c for c in cs
                       if any(m.lower() in c.lower() for m in TASKLIST_MARKERS)})
    if xaml and not clock:
        verdict = "xaml_only"
    elif clock and xaml:
        verdict = "mixed"
    elif clock:
        verdict = "classic_hookable"
    else:
        verdict = "unknown"
    return {
        "verdict": verdict,
        "classic_clock_windows": clock,
        "xaml_windows": xaml,
        "tasklist_windows": tasklist,
        "classic_surface_count": len(clock),
        "xaml_surface_count": len(xaml),
    }


# --------------------------------------------------------------------------- #
# 纯逻辑：fail-safe 环境真值表（PRD §6.2 唯一「启用」组合）
# --------------------------------------------------------------------------- #

SUPPORTED_ENV: Dict[str, Any] = {
    "current_build": "26300",
    "ubr": 9711,
    "display_version": "26H2",
    "edition": "Professional",
    "edge": "Bottom",
    "monitor_count": 1,
    "auto_hide": False,
    "ttb": "clear",
    "tray_present": True,
}

# 六个判据簇 -> 不匹配原因（对齐 PRD §6.2 / ARCH §4.7）
_ENV_CHECKS: Tuple[Tuple[str, Tuple[str, ...], str], ...] = (
    ("build", ("current_build", "ubr", "display_version", "edition"), "BuildMismatch"),
    ("edge", ("edge",), "EdgeNotBottom"),
    ("monitors", ("monitor_count",), "MultiMonitor"),
    ("autohide", ("auto_hide",), "AutoHideOn"),
    ("ttb", ("ttb",), "TtbNotClear"),
    ("tree", ("tray_present",), "TrayMissing"),
)


@dataclass
class EnvSnapshot:
    """环境快照。None 表示「探测取不到值」，按 PRD 契约一律走安全方向（禁用）。"""
    current_build: Optional[str] = None
    ubr: Optional[int] = None
    display_version: Optional[str] = None
    edition: Optional[str] = None
    edge: Optional[str] = None
    monitor_count: Optional[int] = None
    auto_hide: Optional[bool] = None
    ttb: Optional[str] = None
    tray_present: Optional[bool] = None
    unknown_fields: List[str] = field(default_factory=list)
    source: str = "unknown"

    def get(self, key: str) -> Any:
        return getattr(self, key, None)


@dataclass
class SupportVerdict:
    disabled: bool
    reason: str = ""
    mismatched: List[str] = field(default_factory=list)


def supported_snapshot() -> EnvSnapshot:
    """构造唯一「启用」组合（供真值表与 Fake 使用）。"""
    return EnvSnapshot(
        current_build=SUPPORTED_ENV["current_build"],
        ubr=SUPPORTED_ENV["ubr"],
        display_version=SUPPORTED_ENV["display_version"],
        edition=SUPPORTED_ENV["edition"],
        edge=SUPPORTED_ENV["edge"],
        monitor_count=SUPPORTED_ENV["monitor_count"],
        auto_hide=SUPPORTED_ENV["auto_hide"],
        ttb=SUPPORTED_ENV["ttb"],
        tray_present=SUPPORTED_ENV["tray_present"],
        source="test-double",
    )


def evaluate_env(snap: EnvSnapshot) -> SupportVerdict:
    """纯函数：逐项比对支持矩阵；任一项不匹配 / 取不到值一律禁用（安全方向）。"""
    mismatched: List[str] = []
    first_reason = ""
    for _cluster, keys, reason in _ENV_CHECKS:
        for key in keys:
            expected = SUPPORTED_ENV[key]
            actual = snap.get(key)
            if actual is None or actual != expected:
                mismatched.append("%s(expected=%r,measured=%r)" % (key, expected, actual))
                if not first_reason:
                    first_reason = reason
                break
    if first_reason:
        return SupportVerdict(disabled=True, reason=first_reason, mismatched=mismatched)
    return SupportVerdict(disabled=False, reason="Supported", mismatched=[])


# --------------------------------------------------------------------------- #
# 纯逻辑：窗口树快照哈希（实验前后比对「无残留 / 无新增窗口」）
# --------------------------------------------------------------------------- #

def tree_snapshot_hash(rows: Sequence[Dict[str, Any]]) -> str:
    """纯函数：窗口树行的规范化 sha256。同样输入必得同哈希，任一字段变化必变。"""
    canon = json.dumps(list(rows), sort_keys=True, ensure_ascii=False,
                       separators=(",", ":"))
    return hashlib.sha256(canon.encode("utf-8")).hexdigest()


# --------------------------------------------------------------------------- #
# 纯逻辑：红线源码静态断言（分片 token 定义，见模块 docstring 纪律 4）
# --------------------------------------------------------------------------- #

def build_redline_rules() -> List[Tuple[str, str]]:
    """构造红线禁用 token 表：(完整 token, 中文说明)。

    所有 token 均以「分片拼接」形式给出，使本文件源码文本中不出现任何完整的
    禁用 API 名连续字面量；运行时拼接结果仍是完整 token，可正常参与扫描。
    注意：注入必需但只读 / 可卸载的 API（进程内存写入、远程线程、库加载、
    库释放、模块快照、注册表只读查询等）**不在**本表，否则阶段 1 合法代码
    会被误判。本表只锁「会改窗口 / 改颜色 / 改系统 / 装 hook / 强杀 / 换
    窗口过程 / 写注册表 / 替换系统资源」的破坏性动作。
    """
    parts: List[Tuple[str, str]] = [
        ("SetWindow" + "Subclass", "窗口子类化（安装）"),
        ("SetWindow" + "LongPtr", "替换窗口过程指针（含 A/W 变体）"),
        ("SetWindow" + "Long", "替换窗口过程指针（旧名）"),
        ("SetClass" + "LongPtr", "替换窗口类过程指针"),
        ("GWLP_WND" + "PROC", "窗口过程替换常量"),
        ("GCLP_WND" + "PROC", "窗口类过程替换常量"),
        ("SetWindowsHook" + "Ex", "安装消息钩子"),
        ("SetWinEvent" + "Hook", "安装事件钩子"),
        ("DefSubclass" + "Proc", "子类化链调用"),
        ("SetText" + "Color", "写文字前景色"),
        ("SetBk" + "Color", "写背景色"),
        ("SetDCBrush" + "Color", "写 DC 画刷颜色"),
        ("SetDCPen" + "Color", "写 DC 画笔颜色"),
        ("SetLayeredWindow" + "Attributes", "写分层窗口 alpha / 颜色键"),
        ("SetWindowComposition" + "Attribute", "写未文档化合成属性（背景）"),
        ("DwmSetWindow" + "Attribute", "写 DWM 窗口属性"),
        ("UpdateLayered" + "Window", "写分层窗口像素"),
        ("SetWindow" + "Theme", "per-HWND uxtheme 改写"),
        ("Accent" + "Policy", "ACCENT 合成策略写入"),
        ("Set" + "Parent", "改写窗口父级"),
        ("Terminate" + "Process", "强制结束进程"),
        ("task" + "kill", "命令行强杀（含重启 Explorer 意图）"),
        ("Stop-" + "Process", "PowerShell 强杀进程"),
        ("RegSet" + "Value", "注册表写入（含 Ex 变体）"),
        ("RegCreate" + "Key", "注册表键创建"),
        ("RegDelete" + "Key", "注册表键删除"),
        ("RegDelete" + "Value", "注册表值删除"),
        ("RegReplace" + "Key", "注册表键替换"),
        ("RegSave" + "Key", "注册表键导出写"),
        ("RegSetKey" + "Security", "注册表安全描述符写"),
        ("SystemUses" + "LightTheme", "全局主题注册表写入意图"),
        ("AppsUse" + "LightTheme", "全局主题注册表写入意图"),
        ("Color" + "Prevalence", "强调色启用写入意图"),
        ("Accent" + "Color", "强调色写入意图"),
        ("Move" + "File", "系统资源替换（含 Ex / WithProgress 变体）"),
        ("Replace" + "File", "系统资源替换"),
    ]
    return [(token, desc) for token, desc in parts]


def required_redline_tokens() -> List[str]:
    """PRD §9 / ARCH §4.9 必查 token 清单（P1：硬编码，锁规则表绝对内容）。

    同样采用分片拼接，避免本文件源码文本出现完整禁用 API 名而自我命中。
    这份清单与 build_redline_rules 相互独立：删规则 / 削表都会让
    assert_required_redline_tokens 变红。
    """
    parts = (
        "SetWindow" + "Subclass",
        "SetWindow" + "LongPtr",
        "SetWindowsHook" + "Ex",
        "SetText" + "Color",
        "DwmSetWindow" + "Attribute",
        "SetWindowComposition" + "Attribute",
        "Terminate" + "Process",
        "RegSet" + "Value",
        "Move" + "File",
        "Accent" + "Policy",
        "SystemUses" + "LightTheme",
    )
    return list(parts)


def assert_required_redline_tokens(rules: Sequence[Tuple[str, str]]) -> bool:
    """纯函数：断言规则表含全部必查 token 且条数不低于下限（P1）。

    QA 曾把 36 条削到 1 条仍全绿 —— 本断言锁的是绝对内容而非相对数量。
    """
    tokens = {token for token, _desc in rules}
    if len(rules) < MIN_REDLINE_RULE_COUNT:
        return False
    return all(token in tokens for token in required_redline_tokens())


def scan_redlines(text: str, rules: Sequence[Tuple[str, str]]) -> List[Tuple[str, int]]:
    """纯函数：在源码文本里扫描禁用 token（大小写不敏感），返回 (token, 次数)。"""
    lowered = text.lower()
    hits: List[Tuple[str, int]] = []
    for token, _desc in rules:
        count = lowered.count(token.lower())
        if count > 0:
            hits.append((token, count))
    return hits


def _redline_scan_paths() -> List[str]:
    """返回需要被红线扫描的源码文件路径（仅代码文件，不含说明文档）。"""
    paths: List[str] = [_SCRIPT_PATH]
    patterns = [
        os.path.join(_PROBE_DIR, "*.c"),
        os.path.join(_PROBE_DIR, "*.ps1"),
        os.path.join(_PROBE_DIR, "tests", "*.py"),
    ]
    for pattern in patterns:
        paths.extend(sorted(glob.glob(pattern)))
    seen: set = set()
    unique: List[str] = []
    for path in paths:
        key = os.path.normcase(os.path.abspath(path))
        if key not in seen and os.path.isfile(path):
            seen.add(key)
            unique.append(path)
    return unique


def collect_source_text(paths: Sequence[str]) -> Dict[str, str]:
    """纯文件 IO（标准库 open）：读取源码文本，供静态断言使用。

    这是唯一在 self-test 里发生的文件访问 —— 只读文本，不执行、
    不注入、不碰 Win32 / COM / 注册表。
    """
    out: Dict[str, str] = {}
    for path in paths:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            out[path] = handle.read()
    return out


def assert_source_clean(paths: Sequence[str],
                        rules: Sequence[Tuple[str, str]]) -> List[Tuple[str, str, int]]:
    """源码静态断言：所有实验源码零禁用 API。返回命中列表（空 = 干净）。"""
    hits: List[Tuple[str, str, int]] = []
    sources = collect_source_text(paths)
    for path, text in sources.items():
        for token, count in scan_redlines(text, rules):
            hits.append((os.path.basename(path), token, count))
    return hits


# --------------------------------------------------------------------------- #
# 纯逻辑：卸载完整性记账（「每一笔 install 都有对应 uninstall」）
# --------------------------------------------------------------------------- #

class InstallJournal:
    """记录 install / uninstall 动作，支撑 V11「逐笔可还原」断言。"""

    def __init__(self) -> None:
        self.installs: List[str] = []
        self.uninstalls: List[str] = []

    def install(self, op: str) -> None:
        self.installs.append(op)

    def uninstall(self, op: str) -> None:
        self.uninstalls.append(op)

    def is_balanced(self) -> bool:
        """每一笔 install 都有对应 uninstall（按 op 计数完全相等）。"""
        return Counter(self.installs) == Counter(self.uninstalls)

    def assert_balanced(self) -> None:
        if not self.is_balanced():
            raise AssertionError(
                "uninstall incomplete: installs=%r uninstalls=%r"
                % (self.installs, self.uninstalls))


# --------------------------------------------------------------------------- #
# 纯逻辑：看门狗状态机 + 可注入后端（真实阶段 1 与 Fake 共用）
# --------------------------------------------------------------------------- #

OUTCOME_SUCCESS = "success"
OUTCOME_DISABLED_FAILSAFE = "disabled_failsafe"
OUTCOME_ABORTED = "aborted"

PHASE_NORMAL = "normal"
PHASE_TIMEOUT = "timeout"
PHASE_CRASH = "crash"
PHASE_FAILSAFE = "failsafe_mismatch"


@dataclass
class RunResult:
    outcome: str
    phase: str
    exit_code: int
    journal_balanced: bool
    install_count: int
    uninstall_count: int
    uninstall_attempted: bool
    verify_ok: bool
    restart_explorer_attempted: bool
    retry_inject_count: int
    events: List[Tuple[str, Dict[str, Any]]] = field(default_factory=list)


class ExperimentBackend:
    """看门狗依赖的后端接口。真实实现见 RealInjectBackend，测试替身见 FakeBackend。

    所有方法都被 WatchdogFSM.run 以固定顺序调用；子类不得改变签名。
    """

    def probe_env(self) -> EnvSnapshot:
        raise NotImplementedError

    def install(self, op: str) -> bool:
        raise NotImplementedError

    def wait(self, timeout_s: float) -> str:
        """返回 'finished' | 'timeout' | 'crash'。"""
        raise NotImplementedError

    def uninstall(self, op: str) -> bool:
        raise NotImplementedError

    def verify(self) -> bool:
        raise NotImplementedError

    def log(self, event: str, **kv: Any) -> None:
        raise NotImplementedError


class WatchdogFSM:
    """实验监督状态机（纯逻辑，系统副作用全部经 backend 注入）。

    不变量：
      * fail-safe 不匹配 -> 零 install、零 uninstall、只 1 条 WARN、正常禁用退出；
      * 只要 install 过，无论 normal / timeout / crash 都必走 uninstall + verify；
      * 任何失败都不得自动重启 Explorer、不得自动重试注入；
      * 卸载完整性由 InstallJournal 断言（每笔 install 都有对应 uninstall）。
    """

    def __init__(self, journal: Optional[InstallJournal] = None) -> None:
        self.journal = journal or InstallJournal()

    def run(self, backend: ExperimentBackend, timeout_s: float) -> RunResult:
        # 记账职责统一交给 backend（真实后端与 Fake 语义一致），FSM 只读
        # backend 的 journal 做「每笔 install 都有对应 uninstall」断言。
        journal = getattr(backend, "journal", self.journal)
        snap = backend.probe_env()
        verdict = evaluate_env(snap)

        def safe_log(event: str, **kv: Any) -> None:
            """日志后端故障绝不中断实验（P3）。"""
            try:
                backend.log(event, **kv)
            except Exception:
                pass

        # 路径 4 / 4：FailSafe 不匹配 —— 绝不安装、绝不注入。
        if verdict.disabled:
            safe_log("g9.experiment.failsafe", phase="failsafe",
                     reason=verdict.reason, hook_installed=False, writes=0)
            return RunResult(
                outcome=OUTCOME_DISABLED_FAILSAFE, phase=PHASE_FAILSAFE,
                exit_code=0, journal_balanced=journal.is_balanced(),
                install_count=len(journal.installs),
                uninstall_count=len(journal.uninstalls),
                uninstall_attempted=False, verify_ok=True,
                restart_explorer_attempted=False, retry_inject_count=0,
                events=getattr(backend, "events", []),
            )

        safe_log("g9.experiment.armed", phase="arm", timeout_s=timeout_s)
        install_started = False
        wait_result = "crash"
        install_exception = ""
        verify_ok = False
        try:
            install_started = True
            # P7：收紧契约 —— install 返回 False 即视为失败，不再调用 wait。
            if backend.install("readonly-dll") is False:
                wait_result = "crash"
                install_exception = "install_returned_false"
            else:
                wait_result = backend.wait(timeout_s)
        except Exception as exc:
            # install / wait 内部异常（含日志故障）不得冒泡中断卸载流程（P3）。
            wait_result = "crash"
            install_exception = type(exc).__name__
            safe_log("g9.experiment.watchdog", phase="install_exception",
                     reason=install_exception, action="trigger_uninstall")
        finally:
            # 只要 install 已发起，无论正常 / 超时 / 异常 / 崩溃，都必走卸载 + 校验。
            if install_started:
                try:
                    backend.uninstall("readonly-dll")
                except Exception as exc:
                    safe_log("g9.experiment.uninstall", phase="phase1",
                             action="uninstall_exception",
                             reason=type(exc).__name__)
                try:
                    verify_ok = backend.verify()
                except Exception:
                    verify_ok = False

        if wait_result == "finished":
            phase = PHASE_NORMAL
            aborted = False
        elif wait_result == "timeout":
            phase = PHASE_TIMEOUT
            aborted = True
            safe_log("g9.experiment.watchdog", phase="timeout",
                     timeout_s=timeout_s, action="trigger_uninstall")
        else:
            phase = PHASE_CRASH
            aborted = True
            safe_log("g9.experiment.watchdog", phase="crash",
                     action="trigger_uninstall")

        balanced = journal.is_balanced()
        if not balanced:
            journal.assert_balanced()  # 必抛；接线断言失败即变红

        if aborted or not verify_ok or install_exception:
            outcome = OUTCOME_ABORTED
            exit_code = 1
            safe_log("g9.experiment.end", phase=phase, outcome=outcome,
                     verify_ok=verify_ok, exit_code=exit_code,
                     reason=install_exception or "")
        else:
            outcome = OUTCOME_SUCCESS
            exit_code = 0
            safe_log("g9.experiment.end", phase=phase, outcome=outcome,
                     verify_ok=verify_ok, exit_code=exit_code)

        return RunResult(
            outcome=outcome, phase=phase, exit_code=exit_code,
            journal_balanced=balanced,
            install_count=len(journal.installs),
            uninstall_count=len(journal.uninstalls),
            uninstall_attempted=install_started, verify_ok=verify_ok,
            restart_explorer_attempted=False, retry_inject_count=0,
            events=getattr(backend, "events", []),
        )


# --------------------------------------------------------------------------- #
# 测试替身（Fake，零真机 Explorer 依赖、零系统 API）
# --------------------------------------------------------------------------- #

class FakeBackend(ExperimentBackend):
    """看门狗测试替身：wait 结果、verify 结果、环境快照均可注入。"""

    def __init__(self, snap: EnvSnapshot, wait_result: str = "finished",
                 verify_ok: bool = True, uninstall_succeeds: bool = True,
                 skip_uninstall_journal: bool = False) -> None:
        self.snap = snap
        self.wait_result = wait_result
        self.verify_result = verify_ok
        self.uninstall_succeeds = uninstall_succeeds
        self.skip_uninstall_journal = skip_uninstall_journal
        self.journal = InstallJournal()
        self.events: List[Tuple[str, Dict[str, Any]]] = []
        self.install_calls = 0
        self.uninstall_calls = 0
        self.wait_calls = 0

    def probe_env(self) -> EnvSnapshot:
        return self.snap

    def install(self, op: str) -> bool:
        self.install_calls += 1
        self.journal.install(op)
        return True

    def wait(self, timeout_s: float) -> str:
        self.wait_calls += 1
        return self.wait_result

    def uninstall(self, op: str) -> bool:
        self.uninstall_calls += 1
        if not self.skip_uninstall_journal:
            self.journal.uninstall(op)
        return self.uninstall_succeeds

    def verify(self) -> bool:
        return self.verify_result

    def log(self, event: str, **kv: Any) -> None:
        self.events.append((event, kv))


def _fsm_with_fake(snap: EnvSnapshot, wait_result: str = "finished",
                   verify_ok: bool = True) -> Tuple[RunResult, FakeBackend]:
    """把 FSM 的 journal 与 Fake 的 journal 合用，便于断言卸载完整性。"""
    fake = FakeBackend(snap, wait_result=wait_result, verify_ok=verify_ok)
    fsm = WatchdogFSM(journal=fake.journal)
    return fsm.run(fake, DEFAULT_TIMEOUT_S), fake


# --------------------------------------------------------------------------- #
# 结构化日志（RecordingLogger 供 self-test；StructuredLogger 供真机实验）
# --------------------------------------------------------------------------- #

def _fmt_value(value: Any) -> str:
    """把值格式化为不含空格 / 换行的 key=value 片段（对齐项目 log.rs 口径）。"""
    text = str(value)
    return text.replace(" ", "_").replace("\n", "\\n").replace("\r", "")


class RecordingLogger:
    """测试用日志替身：只记内存，零文件写入、零系统 API。"""

    def __init__(self) -> None:
        self.records: List[Tuple[str, Dict[str, Any]]] = []

    def emit(self, event: str, **kv: Any) -> None:
        self.records.append((event, dict(kv)))

    def line(self, event: str, **kv: Any) -> str:
        parts = ["event=" + event]
        parts.extend("%s=%s" % (k, _fmt_value(v)) for k, v in sorted(kv.items()))
        return " ".join(parts)


class StructuredLogger:
    """真机实验日志：写 stdout，可选写文件（--log-file 显式才写）。"""

    def __init__(self, log_file: Optional[str] = None) -> None:
        self.log_file = log_file
        self.records: List[Tuple[str, Dict[str, Any]]] = []
        self.started = time.monotonic()

    def emit(self, event: str, **kv: Any) -> None:
        record = dict(kv)
        record.setdefault("elapsed_ms", int((time.monotonic() - self.started) * 1000))
        self.records.append((event, record))
        parts = ["event=" + event]
        parts.extend("%s=%s" % (k, _fmt_value(v)) for k, v in sorted(record.items()))
        line = " ".join(parts)
        print(line, flush=True)
        if self.log_file:
            try:
                with open(self.log_file, "a", encoding="utf-8") as handle:
                    handle.write(line + "\n")
            except Exception:
                # 日志故障降级：stdout 已输出，补 1 条 WARN，绝不中断实验（P3）。
                print("event=%s.log_write_failed level=WARN action=degrade_to_stdout "
                      "log_file=%s" % (EVENT_PREFIX, _fmt_value(self.log_file)),
                      flush=True)


# --------------------------------------------------------------------------- #
# 真实系统层（惰性加载；self-test 绝不调用，见模块 docstring 纪律 1）
# --------------------------------------------------------------------------- #

class GUID(ctypes.Structure):
    """Win32 GUID（COM 类 / 接口标识）。"""
    _fields_ = [
        ("Data1", ctypes.c_ulong),
        ("Data2", ctypes.c_ushort),
        ("Data3", ctypes.c_ushort),
        ("Data4", ctypes.c_ubyte * 8),
    ]


class MODULEENTRY32W(ctypes.Structure):
    """tlhelp32 MODULEENTRY32W（用于取目标进程里的 DLL 基址）。"""
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("th32ModuleID", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("GlblcntUsage", wintypes.DWORD),
        ("ProccntUsage", wintypes.DWORD),
        ("modBaseAddr", ctypes.POINTER(ctypes.c_byte)),
        ("modBaseSize", wintypes.DWORD),
        ("hModule", wintypes.HMODULE),
        ("szModule", wintypes.WCHAR * 256),
        ("szExePath", wintypes.WCHAR * 512),
    ]


WNDENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)


class Win32Binding:
    """集中惰性绑定 Win32 / COM 符号；构造即标记「已触碰系统 API」。"""

    def __init__(self) -> None:
        _touch_system_api()
        self.user32 = ctypes.WinDLL("user32", use_last_error=True)
        self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self.ole32 = ctypes.WinDLL("ole32", use_last_error=True)
        self.shell32 = ctypes.WinDLL("shell32", use_last_error=True)
        self._bind()

    def _bind(self) -> None:
        u, k, o, s = self.user32, self.kernel32, self.ole32, self.shell32

        u.FindWindowW.restype = wintypes.HWND
        u.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
        u.GetClassNameW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        u.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
        u.IsWindowVisible.argtypes = [wintypes.HWND]
        u.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
        u.GetWindowThreadProcessId.argtypes = [wintypes.HWND,
                                               ctypes.POINTER(wintypes.DWORD)]
        u.GetParent.restype = wintypes.HWND
        u.GetParent.argtypes = [wintypes.HWND]
        u.EnumChildWindows.restype = wintypes.BOOL
        u.EnumChildWindows.argtypes = [wintypes.HWND, WNDENUMPROC, wintypes.LPARAM]
        u.GetSystemMetrics.argtypes = [ctypes.c_int]
        u.GetSystemMetrics.restype = ctypes.c_int

        k.OpenProcess.restype = wintypes.HANDLE
        k.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        k.VirtualAllocEx.restype = wintypes.LPVOID
        k.VirtualAllocEx.argtypes = [wintypes.HANDLE, wintypes.LPVOID,
                                     ctypes.c_size_t, wintypes.DWORD, wintypes.DWORD]
        k.VirtualFreeEx.restype = wintypes.BOOL
        k.VirtualFreeEx.argtypes = [wintypes.HANDLE, wintypes.LPVOID,
                                    ctypes.c_size_t, wintypes.DWORD]
        k.WriteProcessMemory.restype = wintypes.BOOL
        k.WriteProcessMemory.argtypes = [wintypes.HANDLE, wintypes.LPVOID,
                                         wintypes.LPCVOID, ctypes.c_size_t,
                                         ctypes.POINTER(ctypes.c_size_t)]
        k.ReadProcessMemory.restype = wintypes.BOOL
        k.CreateRemoteThread.restype = wintypes.HANDLE
        k.CreateRemoteThread.argtypes = [wintypes.HANDLE, wintypes.LPVOID,
                                         ctypes.c_size_t, wintypes.LPVOID,
                                         wintypes.LPVOID, wintypes.DWORD,
                                         wintypes.LPDWORD]
        k.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        k.WaitForSingleObject.restype = wintypes.DWORD
        k.GetExitCodeThread.argtypes = [wintypes.HANDLE, wintypes.LPDWORD]
        k.GetExitCodeThread.restype = wintypes.BOOL
        k.GetExitCodeProcess.argtypes = [wintypes.HANDLE, wintypes.LPDWORD]
        k.GetExitCodeProcess.restype = wintypes.BOOL
        k.CloseHandle.argtypes = [wintypes.HANDLE]
        k.CloseHandle.restype = wintypes.BOOL
        k.LoadLibraryW.restype = wintypes.HMODULE
        k.LoadLibraryW.argtypes = [wintypes.LPCWSTR]
        k.FreeLibrary.argtypes = [wintypes.HMODULE]
        k.FreeLibrary.restype = wintypes.BOOL
        k.GetProcAddress.restype = wintypes.LPVOID
        k.GetProcAddress.argtypes = [wintypes.HMODULE, wintypes.LPCSTR]
        k.GetModuleHandleW.restype = wintypes.HMODULE
        k.GetModuleHandleW.argtypes = [wintypes.LPCWSTR]
        k.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        k.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
        k.Module32FirstW.argtypes = [wintypes.HANDLE,
                                     ctypes.POINTER(MODULEENTRY32W)]
        k.Module32FirstW.restype = wintypes.BOOL
        k.Module32NextW.argtypes = [wintypes.HANDLE,
                                    ctypes.POINTER(MODULEENTRY32W)]
        k.Module32NextW.restype = wintypes.BOOL
        k.GetLastError.restype = wintypes.DWORD

        o.CoInitializeEx.argtypes = [wintypes.LPVOID, wintypes.DWORD]
        o.CoInitializeEx.restype = ctypes.c_long
        o.CoCreateInstance.argtypes = [ctypes.POINTER(GUID), wintypes.LPVOID,
                                       wintypes.DWORD, ctypes.POINTER(GUID),
                                       ctypes.POINTER(ctypes.c_void_p)]
        o.CoCreateInstance.restype = ctypes.c_long
        o.CoUninitialize.argtypes = []
        o.CoUninitialize.restype = None

        # 注册表只读（本实验绝不写注册表）
        try:
            self.advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
            self.advapi32.RegOpenKeyExW.argtypes = [
                wintypes.HKEY, wintypes.LPCWSTR, wintypes.DWORD,
                wintypes.DWORD, ctypes.POINTER(wintypes.HKEY)]
            self.advapi32.RegOpenKeyExW.restype = ctypes.c_long
            self.advapi32.RegQueryValueExW.argtypes = [
                wintypes.HKEY, wintypes.LPCWSTR, wintypes.LPDWORD,
                wintypes.LPDWORD, wintypes.LPBYTE, wintypes.LPDWORD]
            self.advapi32.RegQueryValueExW.restype = ctypes.c_long
            self.advapi32.RegCloseKey.argtypes = [wintypes.HKEY]
            self.advapi32.RegCloseKey.restype = ctypes.c_long
        except Exception:  # pragma: no cover - 仅影响真实探测，不让导入失败
            self.advapi32 = None


_BINDING: Optional[Win32Binding] = None


def get_binding() -> Win32Binding:
    """惰性取得 Win32 绑定（第一次调用才加载系统 DLL）。"""
    global _BINDING
    if _BINDING is None:
        _BINDING = Win32Binding()
    return _BINDING


# --------------------------------------------------------------------------- #
# 阶段 0：真实只读探测（UIA 能力 + Win32 窗口树 + 环境 + 哈希）
# --------------------------------------------------------------------------- #

def _reg_read_str(binding: Win32Binding, key_path: str, value_name: str) -> Optional[str]:
    """只读注册表字符串值（不写任何键）。"""
    adv = getattr(binding, "advapi32", None)
    if adv is None:
        return None
    HKEY_LOCAL_MACHINE = ctypes.c_void_p(0x80000002)
    KEY_READ = 0x20019
    hkey = wintypes.HKEY()
    hr = adv.RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_path, 0, KEY_READ,
                           ctypes.byref(hkey))
    if hr != 0:
        return None
    try:
        size = wintypes.DWORD(0)
        kind = wintypes.DWORD(0)
        hr = adv.RegQueryValueExW(hkey, value_name, None, ctypes.byref(kind),
                                  None, ctypes.byref(size))
        if hr != 0 or size.value == 0:
            return None
        buf = ctypes.create_unicode_buffer(size.value // 2 + 1)
        hr = adv.RegQueryValueExW(hkey, value_name, None, ctypes.byref(kind),
                                  ctypes.cast(buf, wintypes.LPBYTE),
                                  ctypes.byref(size))
        if hr != 0:
            return None
        return buf.value
    finally:
        adv.RegCloseKey(hkey)


def _reg_read_dword(binding: Win32Binding, key_path: str,
                    value_name: str) -> Optional[int]:
    """只读注册表 DWORD 值（不写任何键）。"""
    adv = getattr(binding, "advapi32", None)
    if adv is None:
        return None
    HKEY_LOCAL_MACHINE = ctypes.c_void_p(0x80000002)
    KEY_READ = 0x20019
    hkey = wintypes.HKEY()
    hr = adv.RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_path, 0, KEY_READ,
                           ctypes.byref(hkey))
    if hr != 0:
        return None
    try:
        data = wintypes.DWORD(0)
        size = wintypes.DWORD(ctypes.sizeof(data))
        kind = wintypes.DWORD(0)
        hr = adv.RegQueryValueExW(hkey, value_name, None, ctypes.byref(kind),
                                  ctypes.cast(ctypes.byref(data), wintypes.LPBYTE),
                                  ctypes.byref(size))
        if hr != 0:
            return None
        return int(data.value)
    finally:
        adv.RegCloseKey(hkey)


def probe_env_real(tray_present: bool, edge: Optional[str],
                   monitor_count: Optional[int]) -> EnvSnapshot:
    """真实环境探测（全部只读）：OS Build / 显示器数 / 任务栏方位 / 窗口树存在性。

    如实标注：TranslucentTB 桌面态 clear 与自动隐藏的只读探测属组② 范围
    （ARCH §7.3 A4），本实验程序不含该探测，故这两项为 unknown，evaluate_env
    会按 PRD 契约走安全方向（不匹配即禁用）。主理人可用 --env-json 传入
    已核对的环境快照（日志会记 env_source=operator-supplied，不伪造）。
    """
    b = get_binding()
    base = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
    current_build = _reg_read_str(b, base, "CurrentBuild")
    display_version = _reg_read_str(b, base, "DisplayVersion")
    edition = _reg_read_str(b, base, "EditionID")
    ubr = _reg_read_dword(b, base, "UBR")
    if monitor_count is None:
        monitor_count = int(b.user32.GetSystemMetrics(80))  # SM_CMONITORS
    snap = EnvSnapshot(
        current_build=current_build, ubr=ubr,
        display_version=display_version, edition=edition,
        edge=edge, monitor_count=monitor_count,
        auto_hide=None, ttb=None, tray_present=tray_present,
        unknown_fields=["auto_hide", "ttb"],
        source="live-readonly",
    )
    return snap


def enumerate_tray_rows(binding: Win32Binding) -> Tuple[Optional[Dict[str, Any]],
                                                        List[Dict[str, Any]], str]:
    """只读枚举 Shell_TrayWnd 及全部后代（复用组① 探针的只读思路，独立实现）。"""
    u = binding.user32
    root = u.FindWindowW("Shell_TrayWnd", None)
    if not root:
        return None, [], "Shell_TrayWnd 不存在（Explorer 可能正在重启）"

    def class_of(hwnd: int) -> str:
        buf = ctypes.create_unicode_buffer(256)
        u.GetClassNameW(hwnd, buf, 256)
        return buf.value

    def title_of(hwnd: int) -> str:
        buf = ctypes.create_unicode_buffer(512)
        u.GetWindowTextW(hwnd, buf, 512)
        return buf.value

    def rect_of(hwnd: int) -> List[int]:
        r = wintypes.RECT()
        u.GetWindowRect(hwnd, ctypes.byref(r))
        return [r.left, r.top, r.right, r.bottom]

    def pid_of(hwnd: int) -> int:
        pid = wintypes.DWORD()
        u.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        return pid.value

    root_row = {
        "class": class_of(root), "title": title_of(root),
        "visible": bool(u.IsWindowVisible(root)), "rect": rect_of(root),
        "pid": pid_of(root), "parent_class": "", "hwnd": "0x%X" % root,
    }
    rows: List[Dict[str, Any]] = []

    def cb(hwnd: int, _lp: int) -> bool:
        parent = u.GetParent(hwnd)
        rows.append({
            "class": class_of(hwnd), "title": title_of(hwnd),
            "visible": bool(u.IsWindowVisible(hwnd)), "rect": rect_of(hwnd),
            "pid": pid_of(hwnd),
            "parent_class": class_of(parent) if parent else "",
            "hwnd": "0x%X" % hwnd,
        })
        return True

    proc = WNDENUMPROC(cb)
    ok = u.EnumChildWindows(root, proc, 0)
    note = "" if ok else "EnumChildWindows 返回 FALSE（部分后代可能未枚举）"
    return root_row, rows, note


def _edge_from_rect(rect: Optional[Sequence[int]], screen_h: int) -> Optional[str]:
    """由任务栏矩形推断方位（只读，不依赖 shell32 结构）。"""
    if not rect or len(rect) < 4:
        return None
    _left, top, _right, bottom = rect[0], rect[1], rect[2], rect[3]
    if top > 0 and bottom >= screen_h - 1:
        return "Bottom"
    if top <= 0 and bottom < screen_h:
        return "Top"
    return "Other"


def uia_capability_probe(binding: Win32Binding,
                         tray_hwnd: Optional[int]) -> Dict[str, Any]:
    """UIA COM 客户端「能力」探测（只读、零写入、零注入）。

    只做可靠且可解释的 COM 初始化与对象实例化，并 best-effort 尝试从
    Shell_TrayWnd 的 HWND 取根自动化元素（仅判断接口指针是否非空，不解析
    其属性）。完整自动化树遍历（Name / ControlType / AutomationId /
    ClassName / BoundingRectangle / Pattern 逐节点采集）本次**不执行**，
    如实记录 uia_traversal_completed=false 与原因；uia_nodes 恒为空且
    不代表「未找到目标元素」，仅代表「本次未执行遍历」。

    关于「是否暴露可写的前景 / 颜色属性」：UIA 客户端是只读 consumer，
    其接口只提供属性读取，不提供任何属性 setter —— 这是 API 设计事实；
    本机 XAML 岛 provider 具体暴露哪些自定义属性，本次未完成遍历，属未知。
    """
    o = binding.ole32
    report: Dict[str, Any] = {
        "uia_path": "com-client-ctypes",
        "com_initialized": False,
        "com_initialize_hr": None,
        "automation_created": False,
        "automation_create_hr": None,
        "element_from_handle": False,
        "element_from_handle_hr": None,
        "element_from_handle_note": "",
        "uia_traversal_completed": False,
        "uia_traversal_blocker": (
            "零第三方依赖 + 禁 Add-Type 约束下，纯 ctypes 手工 COM 需精确复刻 "
            "IUIAutomation / IUIAutomationElement / IUIAutomationTreeWalker 的"
            "数十个 vtable 槽位及 VARIANT / SAFEARRAY / BSTR 编组；为避免产生"
            "不可核验的伪观察，本受控实验不执行完整树遍历。"
        ),
        "uia_nodes": [],
        "uia_nodes_note": (
            "空列表表示未执行遍历，绝不表示三类元素在 UIA 树中不存在；"
            "本程序不伪造任何节点命中结果。"
        ),
        "writable_foreground_entry_exposed": False,
        "writable_foreground_note": (
            "【API 设计事实】UIAutomationClient 是只读 consumer，仅提供属性"
            "读取（getter），不提供任何属性写入（setter），故 UIA 客户端路径"
            "不存在可写的前景 / 颜色入口；【未知】本机 XAML 岛 provider 是否"
            "暴露前景 / 颜色类自定义属性，本次未完成遍历，不作结论。"
        ),
    }

    # CLSID_CUIAutomation / IID_IUIAutomation（UIAutomationClient 标准标识）
    clsid = GUID(0xFF48DBA4, 0x60EF, 0x4201,
                 (ctypes.c_ubyte * 8)(0xAA, 0x87, 0x54, 0x10, 0x3E, 0xED, 0x59, 0x4B))
    iid = GUID(0x30CBE57D, 0xD9D0, 0x452A,
               (ctypes.c_ubyte * 8)(0xAB, 0x13, 0x7A, 0xC5, 0xAC, 0x48, 0x25, 0xEE))

    hr = o.CoInitializeEx(None, COINIT_APARTMENTTHREADED)
    report["com_initialize_hr"] = int(hr) & 0xFFFFFFFF
    # S_OK(0) 或 S_FALSE(1) 均视为初始化成功；RPC_E_CHANGED_MODE 也继续 best-effort。
    report["com_initialized"] = hr in (0, 1)
    initialized_here = hr in (0, 1)

    automation = ctypes.c_void_p()
    try:
        hr2 = o.CoCreateInstance(ctypes.byref(clsid), None, CLSCTX_INPROC_SERVER,
                                 ctypes.byref(iid), ctypes.byref(automation))
        report["automation_create_hr"] = int(hr2) & 0xFFFFFFFF
        report["automation_created"] = bool(automation.value) and hr2 >= 0

        if report["automation_created"] and tray_hwnd:
            # IUIAutomation vtable slot 6 = ElementFromHandle(HWND, element**)。
            # 只判断 out 指针是否非空 / HRESULT，不解析元素属性（避免
            # IUIAutomationElement 长 vtable 索引不确定性带来的伪观察）。
            try:
                vtable = ctypes.cast(
                    automation, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))
                ).contents
                fn_ptr = vtable[6]
                proto = ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p,
                                           wintypes.HWND,
                                           ctypes.POINTER(ctypes.c_void_p))
                element = ctypes.c_void_p()
                hr3 = proto(fn_ptr)(automation, tray_hwnd, ctypes.byref(element))
                report["element_from_handle_hr"] = int(hr3) & 0xFFFFFFFF
                report["element_from_handle"] = bool(element.value) and hr3 >= 0
                report["element_from_handle_note"] = (
                    "best-effort：仅判断能否从任务栏 HWND 取得非空 UIA 元素指针，"
                    "未读取其任何属性；结果如实记录，成功也不代表遍历已完成。"
                )
                if element.value:
                    # IUnknown::Release 是 vtable slot 2（COM 二进制契约，确定）。
                    try:
                        evtable = ctypes.cast(
                            element, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))
                        ).contents
                        rel = ctypes.WINFUNCTYPE(ctypes.c_ulong, ctypes.c_void_p)(
                            evtable[2])
                        rel(element)
                    except Exception:
                        pass
            except Exception as exc:
                report["element_from_handle_note"] = (
                    "best-effort 尝试异常（如实记录，不视为成功）：%s" % type(exc).__name__
                )

        if automation.value:
            # IUnknown::Release（slot 2）
            try:
                vtable = ctypes.cast(
                    automation, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))
                ).contents
                rel = ctypes.WINFUNCTYPE(ctypes.c_ulong, ctypes.c_void_p)(vtable[2])
                rel(automation)
            except Exception:
                pass
    finally:
        if initialized_here:
            o.CoUninitialize()
    return report


def run_phase0_readonly(logger: StructuredLogger,
                        env_override: Optional[EnvSnapshot] = None
                        ) -> Dict[str, Any]:
    """阶段 0 编排：零注入、零写入的只读探测，返回结构化结果。"""
    logger.emit(EVENT_PREFIX + ".start", phase="phase0", mode="readonly",
                inject=False, writes=0, timeout_s="n/a")
    b = get_binding()
    root, rows, note = enumerate_tray_rows(b)
    all_rows = ([root] if root else []) + rows
    classes = [r["class"] for r in all_rows]
    classification = classify_tree(classes)
    tree_hash = tree_snapshot_hash(all_rows)
    screen_h = int(b.user32.GetSystemMetrics(1))   # SM_CYSCREEN
    edge = _edge_from_rect(root["rect"] if root else None, screen_h)
    monitor_count = int(b.user32.GetSystemMetrics(80))  # SM_CMONITORS
    tray_hwnd = int(root["hwnd"], 16) if root else 0

    env = probe_env_real(tray_present=bool(root), edge=edge,
                         monitor_count=monitor_count)
    env_source = "live-readonly"
    if env_override is not None:
        env = env_override
        env_source = "operator-supplied"
    verdict = evaluate_env(env)
    logger.emit(EVENT_PREFIX + ".env", phase="phase0", env_source=env_source,
                disabled=verdict.disabled, reason=verdict.reason,
                tray_present=bool(root), edge=edge, monitor_count=monitor_count,
                explorer_pid=(root["pid"] if root else 0))

    uia = uia_capability_probe(b, tray_hwnd)
    logger.emit(EVENT_PREFIX + ".phase0.uia", phase="phase0",
                com_initialized=uia["com_initialized"],
                automation_created=uia["automation_created"],
                element_from_handle=uia["element_from_handle"],
                traversal_completed=uia["uia_traversal_completed"],
                node_count=len(uia["uia_nodes"]),
                writable_foreground=uia["writable_foreground_entry_exposed"])
    logger.emit(EVENT_PREFIX + ".phase0.win32", phase="phase0",
                descendant_count=len(rows), tree_snapshot_hash=tree_hash,
                classification=classification["verdict"])

    answer_qa = {
        "status": "partial_readonly",
        "observation": [
            "UIA COM 客户端在本进程能否实例化 / 能否从任务栏 HWND 取得根元素"
            "（真实 HRESULT 见 uia 字段）已如实记录。",
            "Win32 只读窗口树的类名 / 可见性 / 矩形 / pid 已如实记录。",
        ],
        "not_observed": [
            "未执行 UIA 完整自动化树遍历，故三类元素的 Name / ControlType / "
            "AutomationId / ClassName / BoundingRectangle / Pattern 均未采集；"
            "uia_nodes 为空不代表目标元素不存在。",
            "未做任何写入探测，因此「是否存在可写的前景 / 颜色入口」在真机上"
            "仍属未知；仅给出 UIA consumer 只读的 API 设计事实。",
            "未注入，故 XAML / Composition 视觉树内部节点类型未观测。",
        ],
        "inference": [
            "UIA 客户端接口只读，不构成可写前景色入口（API 设计事实，非本机观察）。",
        ],
    }
    answer_qb = {
        "status": "not_executed",
        "note": "阶段 0 零注入，未加载任何 DLL 到 Explorer；卸载完整性（Q-b）"
                "需阶段 1 真机实验产出，本阶段不伪造任何卸载结果。",
    }
    report = {
        "experiment": EXPERIMENT_ID,
        "mode": "phase0-readonly",
        "inject": False,
        "writes": 0,
        "timeout_s": "n/a",
        "environment": {
            "source": env_source,
            "snapshot": _env_as_dict(env),
            "verdict": {"disabled": verdict.disabled, "reason": verdict.reason,
                        "mismatched": verdict.mismatched},
        },
        "classification": classification,
        "win32_tree": {
            "tray_root": root, "descendant_count": len(rows),
            "descendants": rows, "tree_snapshot_hash": tree_hash, "note": note,
        },
        "uia": uia,
        "answers": {"Q-a": answer_qa, "Q-b": answer_qb},
        "limits": [
            "阶段 0 只读：不注入、不写内存、不改窗口属性、不写注册表、不写颜色、不装 hook。",
            "窗口类名不能直接证明绘制路径；XAML / Composition 视觉树内节点无独立 HWND。",
            "UIA 完整树遍历未执行（原因见 uia.uia_traversal_blocker），绝不伪造 UIA 结果。",
            "「可写前景色入口」真机结论需阶段 1 / 主理人另行核对，本阶段只给只读事实。",
        ],
    }
    logger.emit(EVENT_PREFIX + ".end", phase="phase0", outcome="readonly_complete",
                inject=False, writes=0, exit_code=0)
    return report


def _env_as_dict(env: EnvSnapshot) -> Dict[str, Any]:
    """把环境快照转成可 JSON 序列化的字典（纯逻辑）。"""
    keys = ("current_build", "ubr", "display_version", "edition", "edge",
            "monitor_count", "auto_hide", "ttb", "tray_present",
            "unknown_fields", "source")
    return {key: getattr(env, key) for key in keys}


def _env_from_json(data: Dict[str, Any]) -> EnvSnapshot:
    """由主理人已核对的环境快照构造 EnvSnapshot（日志记 operator-supplied）。"""
    allowed = ("current_build", "ubr", "display_version", "edition", "edge",
               "monitor_count", "auto_hide", "ttb", "tray_present")
    kwargs = {key: data[key] for key in allowed if key in data}
    snap = EnvSnapshot(**kwargs)
    snap.source = "operator-supplied"
    snap.unknown_fields = list(data.get("unknown_fields", []))
    return snap


# --------------------------------------------------------------------------- #
# 阶段 1 真实后端：只读注入 + 完整卸载 + 完整性校验（主理人真机执行）
# --------------------------------------------------------------------------- #

def _kernel32_proc_address(binding: Win32Binding, name: bytes) -> int:
    """取 kernel32 导出函数地址（跨进程基址一致，供远程线程入口使用）。"""
    k = binding.kernel32
    hmod = k.GetModuleHandleW("kernel32.dll")
    if not hmod:
        return 0
    addr = k.GetProcAddress(hmod, name)
    return int(addr) if addr else 0


def _snapshot_modules(binding: Win32Binding, pid: int) -> Dict[str, int]:
    """只读枚举目标进程模块名 -> 基址（CreateToolhelp32Snapshot，64 位安全）。"""
    k = binding.kernel32
    handle = k.CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32,
                                        pid)
    if not handle or int(handle) in (-1, 0xFFFFFFFF, 0xFFFFFFFFFFFFFFFF):
        return {}
    result: Dict[str, int] = {}
    try:
        entry = MODULEENTRY32W()
        entry.dwSize = ctypes.sizeof(MODULEENTRY32W)
        ok = k.Module32FirstW(handle, ctypes.byref(entry))
        while ok:
            base = int(ctypes.cast(entry.modBaseAddr, ctypes.c_void_p).value or 0)
            result[str(entry.szModule).lower()] = base
            entry = MODULEENTRY32W()
            entry.dwSize = ctypes.sizeof(MODULEENTRY32W)
            ok = k.Module32NextW(handle, ctypes.byref(entry))
    finally:
        k.CloseHandle(handle)
    return result


def _explorer_alive(binding: Win32Binding, pid: int) -> bool:
    """只读校验 Explorer 进程仍存活（绝不结束任何进程）。"""
    if not pid:
        return False
    k = binding.kernel32
    handle = k.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return False
    try:
        code = wintypes.DWORD(0)
        ok = k.GetExitCodeProcess(handle, ctypes.byref(code))
        return bool(ok) and code.value == STILL_ACTIVE
    finally:
        k.CloseHandle(handle)


def _file_sha256(path: str) -> str:
    """纯文件读：计算文件 sha256（标准库，不碰系统 API、不注入）。"""
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_build_record(data: Dict[str, Any], dll_exists: bool,
                        dll_digest: str, source_exists: bool,
                        source_digest: str) -> Tuple[bool, str]:
    """纯函数：校验编译记录与产物 / 源码摘要（P4）。

    任何一项缺失或不一致一律按安全方向判不可用，绝不静默通过。
    """
    if not dll_exists:
        return False, "dll_missing: 编译产物不存在，按安全方向判不可用"
    if not data.get("compiled", False):
        return False, ("dll_not_compiled: build-result.json compiled=false, "
                       "reason=%s" % data.get("reason", ""))
    expected = str(data.get("sha256", "") or "")
    if not expected:
        return False, ("dll_hash_missing: build-result.json 缺少 sha256，"
                       "按安全方向判不可用")
    if not dll_digest or expected.lower() != dll_digest.lower():
        return False, ("dll_hash_mismatch: expected=%s actual=%s"
                       % (expected, dll_digest))
    expected_src = str(data.get("source_sha256", "") or "")
    if not expected_src:
        return False, ("source_hash_missing: build-result.json 缺少 source_sha256，"
                       "按安全方向判不可用（不静默通过）")
    if not source_exists:
        return False, "probe_c_missing: probe.c 不存在，按安全方向判不可用"
    if not source_digest or expected_src.lower() != source_digest.lower():
        return False, ("source_hash_mismatch: expected=%s actual=%s"
                       % (expected_src, source_digest))
    return True, "dll_present_verified"


def _dll_ready(dll_path: Optional[str] = None,
               build_result_path: Optional[str] = None,
               probe_c_path: Optional[str] = None) -> Tuple[bool, str]:
    """阶段 1 前置：DLL 已编译、且与 build-result.json 的产物 / 源码哈希一致。"""
    dll = dll_path or _DLL_PATH
    build_result = build_result_path or _BUILD_RESULT_PATH
    probe_c = probe_c_path or _PROBE_C_PATH
    if not os.path.isfile(dll):
        return False, ("dll_not_built: %s 不存在；请用 "
                       "scripts/g9_inject_probe/build.ps1 编译，阶段 1 不可执行"
                       "（如实标注，绝不假装跑过）" % dll)
    if not os.path.isfile(build_result):
        return False, ("build_result_missing: %s 不存在，按安全方向判不可用"
                       % build_result)
    try:
        with open(build_result, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except Exception as exc:  # pragma: no cover - 仅影响真机前置检查
        return False, "build_result_unreadable: %s" % type(exc).__name__
    dll_exists = os.path.isfile(dll)
    source_exists = os.path.isfile(probe_c)
    return verify_build_record(
        data, dll_exists,
        _file_sha256(dll) if dll_exists else "",
        source_exists,
        _file_sha256(probe_c) if source_exists else "",
    )


class RealInjectBackend(ExperimentBackend):
    """阶段 1 真实后端：只读注入 + 完整卸载 + 完整性校验。

    硬约束（红线源码静态断言会核对）：本类不装 hook、不替换窗口过程、不写
    颜色、不写注册表、不改合成属性、不强杀进程、不重启 Explorer、不重试注入。
    所有系统接触都集中在 install / wait / uninstall / verify，由看门狗状态机
    以固定顺序调用；任何异常都被隔离，失败只记录并交给 FSM 走非零码退出。
    """

    def __init__(self, logger: StructuredLogger,
                 env_override: Optional[EnvSnapshot], result_path: str) -> None:
        _touch_system_api()
        self.logger = logger
        self.env_override = env_override
        self.result_path = os.path.abspath(result_path)
        self.binding = get_binding()
        self.events: List[Tuple[str, Dict[str, Any]]] = []
        self.journal = InstallJournal()
        self.baseline_rows: List[Dict[str, Any]] = []
        self.baseline_hash = ""
        self.explorer_pid = 0
        self.modules_before: Dict[str, int] = {}
        self.h_process = 0
        self.path_mem = 0
        self.param_mem = 0
        self.remote_base = 0
        self.h_thread = 0
        self.install_error = ""
        self.timed_out = False
        self.freelibrary_skipped = False
        self.dll_was_loaded = False
        self.hresult = 0
        self.uninstall_result = "not_started"
        self.verify_detail: Dict[str, Any] = {}

    def log(self, event: str, **kv: Any) -> None:
        self.events.append((event, dict(kv)))
        try:
            self.logger.emit(event, **kv)
        except Exception:
            # 日志故障不得中断注入 / 卸载流程（P3）：事件仍留在内存 events 里。
            pass

    # -- ExperimentBackend 接口 -------------------------------------------- #

    def probe_env(self) -> EnvSnapshot:
        b = self.binding
        root, rows, _note = enumerate_tray_rows(b)
        self.baseline_rows = ([root] if root else []) + rows
        self.baseline_hash = tree_snapshot_hash(self.baseline_rows)
        self.explorer_pid = int(root["pid"]) if root else 0
        screen_h = int(b.user32.GetSystemMetrics(1))
        edge = _edge_from_rect(root["rect"] if root else None, screen_h)
        monitor_count = int(b.user32.GetSystemMetrics(80))
        self.modules_before = _snapshot_modules(b, self.explorer_pid)
        snap = probe_env_real(tray_present=bool(root), edge=edge,
                              monitor_count=monitor_count)
        if self.env_override is not None:
            snap = self.env_override
        self.log(EVENT_PREFIX + ".baseline", phase="phase1",
                 explorer_pid=self.explorer_pid,
                 tree_snapshot_hash=self.baseline_hash,
                 module_count=len(self.modules_before), env_source=snap.source)
        return snap

    def _alloc_write(self, handle: int, data: bytes, label: str) -> int:
        """在目标进程分配内存并写入字节；返回远程地址（0 表示失败）。"""
        k = self.binding.kernel32
        mem = k.VirtualAllocEx(handle, None, len(data),
                               MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
        if not mem:
            self.hresult = int(ctypes.get_last_error())
            return 0
        buf = ctypes.create_string_buffer(data)
        written = ctypes.c_size_t(0)
        ok = k.WriteProcessMemory(handle, mem,
                                  ctypes.cast(buf, ctypes.c_void_p),
                                  len(data), ctypes.byref(written))
        if not ok:
            self.hresult = int(ctypes.get_last_error())
            k.VirtualFreeEx(handle, mem, 0, MEM_RELEASE)
            return 0
        self.log(EVENT_PREFIX + ".remote_mem", phase="phase1", action=label,
                 bytes=len(data), written=written.value, hresult=0)
        return int(mem)

    def install(self, op: str) -> bool:
        """只读注入：加载 DLL -> 起导出函数远程线程。不做任何写入 / hook。"""
        self.journal.install(op)   # 发起即记账；真实成功与否见结构化日志
        b = self.binding
        k = b.kernel32
        try:
            if not self.explorer_pid:
                self.install_error = "explorer_pid_unavailable"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error)
                return False
            desired = (PROCESS_CREATE_THREAD | PROCESS_VM_OPERATION |
                       PROCESS_VM_WRITE | PROCESS_VM_READ |
                       PROCESS_QUERY_LIMITED_INFORMATION)
            handle = k.OpenProcess(desired, False, self.explorer_pid)
            if not handle:
                self.hresult = int(ctypes.get_last_error())
                self.install_error = "open_process_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error, hresult=self.hresult)
                return False
            self.h_process = int(handle)

            dll_path = os.path.abspath(_DLL_PATH)
            path_bytes = (dll_path + "\x00").encode("utf-16-le")
            self.path_mem = self._alloc_write(self.h_process, path_bytes,
                                              "write_dll_path")
            if not self.path_mem:
                self.install_error = "write_dll_path_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error, hresult=self.hresult)
                return False
            result_bytes = (self.result_path + "\x00").encode("utf-16-le")
            self.param_mem = self._alloc_write(self.h_process, result_bytes,
                                               "write_result_path")
            if not self.param_mem:
                self.install_error = "write_result_path_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error, hresult=self.hresult)
                return False

            load_addr = _kernel32_proc_address(b, b"LoadLibraryW")
            if not load_addr:
                self.install_error = "loadlibrary_address_unavailable"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error)
                return False
            h_load = k.CreateRemoteThread(self.h_process, None, 0, load_addr,
                                          self.path_mem, 0, None)
            if not h_load:
                self.hresult = int(ctypes.get_last_error())
                self.install_error = "create_load_thread_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error, hresult=self.hresult)
                return False
            k.WaitForSingleObject(h_load, 10000)
            k.CloseHandle(h_load)

            modules = _snapshot_modules(b, self.explorer_pid)
            self.remote_base = modules.get(os.path.basename(_DLL_PATH).lower(), 0)
            if not self.remote_base:
                self.install_error = "remote_module_base_not_found"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error)
                return False
            self.dll_was_loaded = True

            # 本地加载同一 DLL 仅用于计算导出函数偏移（DllMain 为空，安全），
            # 算完立即释放；本地加载不等于向 Explorer 注入。
            local_base = k.LoadLibraryW(dll_path)
            if not local_base:
                self.install_error = "local_load_for_offset_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error)
                return False
            local_func = k.GetProcAddress(local_base, b"G9ReadOnlyProbe")
            offset = (int(local_func) if local_func else 0) - int(local_base)
            k.FreeLibrary(local_base)
            if not local_func or offset <= 0:
                self.install_error = "export_offset_invalid"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error)
                return False
            remote_func = self.remote_base + offset

            h_thread = k.CreateRemoteThread(self.h_process, None, 0, remote_func,
                                            self.param_mem, 0, None)
            if not h_thread:
                self.hresult = int(ctypes.get_last_error())
                self.install_error = "create_probe_thread_failed"
                self.log(EVENT_PREFIX + ".inject", phase="phase1",
                         action="install", result="failed",
                         reason=self.install_error, hresult=self.hresult)
                return False
            self.h_thread = int(h_thread)
            self.log(EVENT_PREFIX + ".inject", phase="phase1", action="install",
                     result="ok", dll=os.path.basename(_DLL_PATH),
                     explorer_pid=self.explorer_pid,
                     remote_base="0x%X" % self.remote_base, hresult=0,
                     writes=0, hooks=0)
            return True
        except Exception as exc:  # pragma: no cover - 真机异常隔离
            self.install_error = "install_exception_%s" % type(exc).__name__
            self.log(EVENT_PREFIX + ".inject", phase="phase1", action="install",
                     result="failed", reason=self.install_error,
                     hresult=self.hresult)
            return False

    def wait(self, timeout_s: float) -> str:
        """看门狗硬等待：返回 finished / timeout / crash，绝不重试注入。"""
        k = self.binding.kernel32
        if self.install_error or not self.h_thread:
            self.log(EVENT_PREFIX + ".watchdog", phase="phase1", action="wait",
                     result="crash", reason=self.install_error or "no_thread")
            return "crash"
        rc = k.WaitForSingleObject(self.h_thread, int(timeout_s * 1000))
        if rc == WAIT_OBJECT_0:
            code = wintypes.DWORD(0)
            k.GetExitCodeThread(self.h_thread, ctypes.byref(code))
            self.log(EVENT_PREFIX + ".watchdog", phase="phase1", action="wait",
                     result="finished", thread_exit_code=code.value,
                     hresult=0)
            return "finished"
        if rc == WAIT_TIMEOUT:
            self.timed_out = True
            self.log(EVENT_PREFIX + ".watchdog", phase="phase1", action="wait",
                     result="timeout", timeout_s=timeout_s, hresult=int(rc),
                     note="远程线程未在硬超时内返回；为保护 Explorer，卸载阶段"
                          "将跳过库释放，绝不强制重启 Explorer。")
            return "timeout"
        self.log(EVENT_PREFIX + ".watchdog", phase="phase1", action="wait",
                 result="crash", hresult=int(rc))
        return "crash"

    def uninstall(self, op: str) -> bool:
        """完整卸载：读结果 -> 库释放 -> 远程内存释放 -> 句柄关闭。

        超时路径为保护 Explorer 跳过库释放（远程线程可能仍在 DLL 内执行），
        如实记录未完成项并让 verify 判失败；绝不强制重启 Explorer。
        """
        self.journal.uninstall(op)  # 发起卸载流程即记账（与 install 一一对应）
        k = self.binding.kernel32
        steps: List[str] = []
        try:
            if os.path.isfile(self.result_path):
                with open(self.result_path, "r", encoding="utf-8",
                          errors="replace") as handle:
                    lines = [line.rstrip("\r\n") for line in handle if line.strip()]
                self.log(EVENT_PREFIX + ".result", phase="phase1",
                         action="read_result", lines=len(lines),
                         sample=(" | ".join(lines[:3]))[:200])
                steps.append("read_result")
            else:
                self.log(EVENT_PREFIX + ".result", phase="phase1",
                         action="read_result", lines=0, missing=True,
                         note="结果文件未生成，如实记录，不伪造探测内容。")
                steps.append("result_missing")

            if self.timed_out:
                self.freelibrary_skipped = True
                self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                         action="library_release", result="skipped",
                         reason="远程线程可能仍在 DLL 代码内；此时释放库会让"
                                "Explorer 崩溃。无窗口过程指向本 DLL，故允许"
                                "inactive DLL 残留到 Explorer 自然重启；"
                                "绝不强制重启 Explorer。",
                         freelibrary=False)
            elif self.dll_was_loaded and self.remote_base and self.h_process:
                free_addr = _kernel32_proc_address(self.binding, b"FreeLibrary")
                if free_addr:
                    h_free = k.CreateRemoteThread(self.h_process, None, 0,
                                                  free_addr, self.remote_base,
                                                  0, None)
                    if h_free:
                        k.WaitForSingleObject(h_free, 10000)
                        k.CloseHandle(h_free)
                        steps.append("library_release")
                        self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                                 action="library_release", result="ok",
                                 remote_base="0x%X" % self.remote_base,
                                 hresult=0)
                    else:
                        self.hresult = int(ctypes.get_last_error())
                        self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                                 action="library_release", result="failed",
                                 hresult=self.hresult)
                        steps.append("library_release_failed")
                else:
                    self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                             action="library_release", result="skipped",
                             reason="free_library_address_unavailable")
                    steps.append("library_release_skipped")
            else:
                self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                         action="library_release", result="skipped",
                         reason="DLL 未成功加载，无需释放")
                steps.append("library_release_not_needed")

            for mem in (self.path_mem, self.param_mem):
                if mem and self.h_process:
                    k.VirtualFreeEx(self.h_process, mem, 0, MEM_RELEASE)
            steps.append("remote_mem_free")
            self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                     action="remote_mem_free", result="ok")

            for handle in (self.h_thread, self.h_process):
                if handle:
                    k.CloseHandle(handle)
            steps.append("handles_closed")

            self.uninstall_result = "completed:" + ",".join(steps)
            self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                     action="uninstall", result=self.uninstall_result,
                     freelibrary_skipped=self.freelibrary_skipped,
                     restart_explorer=False, retry_inject=0)
            return not self.freelibrary_skipped
        except Exception as exc:  # pragma: no cover - 真机异常隔离
            self.uninstall_result = "failed:" + type(exc).__name__
            self.log(EVENT_PREFIX + ".uninstall", phase="phase1",
                     action="uninstall", result=self.uninstall_result,
                     restart_explorer=False, retry_inject=0)
            return False

    def verify(self) -> bool:
        """卸载完整性校验：Explorer 存活 + 窗口树哈希一致 + 无残留 / 新增模块。"""
        b = self.binding
        alive = _explorer_alive(b, self.explorer_pid)
        root, rows, _note = enumerate_tray_rows(b)
        after_rows = ([root] if root else []) + rows
        hash_after = tree_snapshot_hash(after_rows)
        tree_ok = hash_after == self.baseline_hash
        modules_after = _snapshot_modules(b, self.explorer_pid)
        dll_name = os.path.basename(_DLL_PATH).lower()
        dll_gone = dll_name not in modules_after
        new_modules = sorted(set(modules_after) - set(self.modules_before))
        no_new = len(new_modules) == 0
        if self.freelibrary_skipped:
            dll_gone = False  # 如实：超时跳过库释放，可能残留，不谎报成功
        ok = bool(alive and tree_ok and dll_gone and no_new)
        self.verify_detail = {
            "explorer_alive": alive,
            "tree_snapshot_hash_before": self.baseline_hash,
            "tree_snapshot_hash_after": hash_after,
            "tree_unchanged": tree_ok,
            "dll_unloaded": dll_gone,
            "new_modules": new_modules,
            "freelibrary_skipped": self.freelibrary_skipped,
            "uninstall_result": self.uninstall_result,
            "restart_explorer": False,
        }
        self.log(EVENT_PREFIX + ".verify", phase="phase1", action="verify",
                 explorer_alive=alive,
                 tree_snapshot_hash_before=self.baseline_hash,
                 tree_snapshot_hash_after=hash_after, tree_unchanged=tree_ok,
                 dll_unloaded=dll_gone,
                 new_modules=",".join(new_modules) or "none",
                 freelibrary_skipped=self.freelibrary_skipped,
                 uninstall_result=self.uninstall_result,
                 restart_explorer=False, result="ok" if ok else "failed")
        return ok


def phase1_report_fields(result: RunResult,
                         result_file_exists: bool) -> Dict[str, Any]:
    """纯函数：由看门狗结果推导报告的 mode / inject / Q-a 状态（P2 诚实化）。

    FailSafe 零注入时 inject 必须为 False、Q-a 必须为 not_executed；
    aborted 时不得谎报「只读探针已执行」。
    """
    injected = result.install_count > 0
    if result.outcome == OUTCOME_DISABLED_FAILSAFE:
        mode = "phase1-failsafe-disabled"
    elif result.outcome == OUTCOME_ABORTED:
        mode = "phase1-aborted"
    else:
        mode = "phase1-readonly-inject"
    if not injected:
        qa_status = "not_executed"
    elif result.phase == PHASE_NORMAL and result_file_exists:
        qa_status = "readonly_probe_executed"
    else:
        qa_status = "incomplete"
    return {"mode": mode, "inject": injected, "qa_status": qa_status}


def run_phase1(logger: StructuredLogger, timeout_s: float,
               env_override: Optional[EnvSnapshot],
               result_path: str) -> Tuple[int, Dict[str, Any]]:
    """阶段 1 编排：前置检查 -> 看门狗状态机（与 Fake 共用）-> 结构化报告。"""
    logger.emit(EVENT_PREFIX + ".start", phase="phase1", mode="readonly-inject",
                inject_requested=True, writes=0, timeout_s=timeout_s,
                operator_ack="--allow-inject")
    ready, reason = _dll_ready()
    if not ready:
        logger.emit(EVENT_PREFIX + ".blocked", phase="phase1", action="precheck",
                    reason=reason, inject=False, exit_code=2,
                    note="阶段 1 不可执行：只读探针 DLL 未编译 / 不可用；"
                         "如实记录，绝不假装跑过，不自动重试注入、不重启 Explorer。")
        report = {
            "experiment": EXPERIMENT_ID, "mode": "phase1-blocked",
            "inject": False, "reason": reason, "exit_code": 2,
            "limits": ["未编译即不可执行，如实标注；不伪造任何真机注入结果。"],
        }
        logger.emit(EVENT_PREFIX + ".end", phase="phase1", outcome="aborted",
                    exit_code=2, restart_explorer=False)
        return 2, report

    backend = RealInjectBackend(logger, env_override, result_path)
    fsm = WatchdogFSM(journal=backend.journal)
    result = fsm.run(backend, timeout_s)
    fields = phase1_report_fields(result,
                                  os.path.isfile(os.path.abspath(result_path)))
    report = {
        "experiment": EXPERIMENT_ID,
        "mode": fields["mode"],
        "inject": fields["inject"],
        "writes": 0,
        "timeout_s": timeout_s,
        "dll": os.path.abspath(_DLL_PATH),
        "result_file": os.path.abspath(result_path),
        "watchdog": {
            "outcome": result.outcome, "phase": result.phase,
            "exit_code": result.exit_code,
            "journal_balanced": result.journal_balanced,
            "install_count": result.install_count,
            "uninstall_count": result.uninstall_count,
            "uninstall_attempted": result.uninstall_attempted,
            "verify_ok": result.verify_ok,
            "restart_explorer_attempted": result.restart_explorer_attempted,
            "retry_inject_count": result.retry_inject_count,
        },
        "baseline": {
            "explorer_pid": backend.explorer_pid,
            "tree_snapshot_hash": backend.baseline_hash,
            "module_count_before": len(backend.modules_before),
        },
        "verify_detail": backend.verify_detail,
        "uninstall_result": backend.uninstall_result,
        "events": [{"event": event, **kv} for event, kv in backend.events],
        "answers": {
            "Q-a": {"status": fields["qa_status"],
                    "note": "进程内只读枚举结果见 result_file；是否暴露可写前景色"
                            "入口不在本实验范围（本实验零写入），不作结论。"},
            "Q-b": {
                "status": ("measured" if result.phase == PHASE_NORMAL
                           and result.verify_ok else
                           "not_executed" if not fields["inject"] else "incomplete"),
                "note": "卸载完整性以 verify_detail / 结构化日志为准；超时与"
                        "异常路径如实记录未完成项，绝不伪造成功。"},
        },
        "limits": [
            "本实验零 hook / 零窗口过程替换 / 零颜色写入 / 零注册表写入 / 零合成属性写入。",
            "任何失败都不自动重启 Explorer、不自动重试注入，只记录并退出非零码。",
            "超时路径为保护 Explorer 会跳过库释放，可能有 inactive DLL 残留到"
            "Explorer 自然重启；该情况一律判非零码并如实记录。",
            "真机执行仅由主理人在代码与自测全绿后进行；工程师回合不执行注入。",
        ],
    }
    return result.exit_code, report


# --------------------------------------------------------------------------- #
# 接线 / 诚实性 源码静态断言（纯函数，供 self-test 正向 + 反向验证）
# --------------------------------------------------------------------------- #

def _function_slice(source_text: str, header: str) -> str:
    """截取某个顶层 def 的函数体文本（到下一个顶层 def 之前），纯字符串操作。"""
    start = source_text.find(header)
    if start < 0:
        return ""
    next_def = source_text.find("\ndef ", start + len(header))
    return source_text[start:next_def] if next_def > 0 else source_text[start:]


def assert_phase1_wiring(source_text: str) -> bool:
    """接线断言：阶段 1 编排确实经看门狗状态机驱动真实后端（锁「测试全绿但没接线」）。"""
    required = ("RealInjectBackend(logger", "WatchdogFSM(journal=",
                ".run(backend, timeout_s)")
    return all(token in source_text for token in required)


def assert_uia_honesty(source_text: str) -> bool:
    """诚实性断言：UIA 未完成时如实标注、节点列表为空、不谎报可写入口。"""
    required = ('"uia_traversal_completed": False',
                '"uia_nodes": []',
                '"writable_foreground_entry_exposed": False',
                '"uia_nodes_note"')
    return all(token in source_text for token in required)


# --------------------------------------------------------------------------- #
# --self-test：真值表 + 反向验证 + 状态机 + 卸载完整性 + 红线断言（零系统 API）
# --------------------------------------------------------------------------- #

def self_test() -> int:
    """纯逻辑自检。全程零系统 API / 零注入 / 零注册表写入 / 零颜色写入。

    唯一的文件访问是用标准库 open() 读源码文本做静态扫描；不执行任何
    Win32 / COM 调用，不构造 RealInjectBackend，不加载任何系统 DLL。
    """
    checks: List[Tuple[str, bool]] = []

    def check(name: str, cond: Any) -> None:
        checks.append((name, bool(cond)))

    # ---- A. 分类真值表（正向，与组① 探针同风格） -------------------------- #
    check("分类真值表：classic 树 -> classic_hookable",
          classify_tree(SAMPLE_CLASSIC)["verdict"] == "classic_hookable")
    check("分类真值表：xaml 树 -> xaml_only",
          classify_tree(SAMPLE_XAML)["verdict"] == "xaml_only")
    check("分类真值表：混合树 -> mixed",
          classify_tree(SAMPLE_MIXED)["verdict"] == "mixed")
    check("分类真值表：空树 -> unknown",
          classify_tree(SAMPLE_EMPTY)["verdict"] == "unknown")
    check("分类真值表：classic 树能列出时钟窗口",
          "TrayClockWClass" in classify_tree(SAMPLE_CLASSIC)["classic_clock_windows"])
    check("分类真值表：xaml 树能列出 >=2 个 XAML 窗口",
          len(classify_tree(SAMPLE_XAML)["xaml_windows"]) >= 2)
    check("分类真值表：大小写不敏感",
          classify_tree(["trayclockwclass"])["verdict"] == "classic_hookable")

    # ---- A'. 分类反向验证（改坏判定表必须变红） ----------------------------- #
    global XAML_MARKERS, CLASSIC_CLOCK_MARKERS
    saved_xaml = XAML_MARKERS
    XAML_MARKERS = ()
    broken_xaml = classify_tree(SAMPLE_XAML)["verdict"]
    XAML_MARKERS = saved_xaml
    check("反向验证①：XAML 判定表改坏后 xaml_only 必须变化",
          broken_xaml != "xaml_only")
    saved_classic = CLASSIC_CLOCK_MARKERS
    CLASSIC_CLOCK_MARKERS = ()
    broken_classic = classify_tree(SAMPLE_CLASSIC)["verdict"]
    CLASSIC_CLOCK_MARKERS = saved_classic
    check("反向验证②：经典判定表改坏后 classic_hookable 必须变化",
          broken_classic != "classic_hookable")

    # ---- B. fail-safe 环境真值表 ------------------------------------------- #
    ok_env = evaluate_env(supported_snapshot())
    check("环境真值表：唯一匹配组合 -> Supported",
          (not ok_env.disabled) and ok_env.reason == "Supported")
    mismatch_cases = (
        ("BuildMismatch", {"current_build": "22631"}),
        ("EdgeNotBottom", {"edge": "Top"}),
        ("MultiMonitor", {"monitor_count": 2}),
        ("AutoHideOn", {"auto_hide": True}),
        ("TtbNotClear", {"ttb": "blur"}),
        ("TrayMissing", {"tray_present": False}),
    )
    for reason, patch in mismatch_cases:
        snap = supported_snapshot()
        for key, value in patch.items():
            setattr(snap, key, value)
        verdict = evaluate_env(snap)
        check("环境真值表：%s -> 禁用 + 原因正确" % reason,
              verdict.disabled and verdict.reason == reason
              and len(verdict.mismatched) > 0)
    unknown_snap = supported_snapshot()
    unknown_snap.ttb = None
    check("环境真值表：探测取不到值（None）按不匹配（安全方向）",
          evaluate_env(unknown_snap).disabled)

    # ---- B'. 环境反向验证 -------------------------------------------------- #
    baseline_snap = supported_snapshot()   # 改坏前先固定一份匹配快照
    saved_expected = SUPPORTED_ENV["monitor_count"]
    SUPPORTED_ENV["monitor_count"] = 2
    broken_env = evaluate_env(baseline_snap)
    SUPPORTED_ENV["monitor_count"] = saved_expected
    check("反向验证③：支持矩阵改坏后原本匹配的快照必须变禁用",
          broken_env.disabled and broken_env.reason == "MultiMonitor")

    # ---- C. 窗口树快照哈希（确定性 + 反向） -------------------------------- #
    sample_rows = [
        {"class": "Shell_TrayWnd", "hwnd": "0x10166", "visible": True},
        {"class": "TrayNotifyWnd", "hwnd": "0x10170", "visible": True},
    ]
    hash_a = tree_snapshot_hash(sample_rows)
    hash_b = tree_snapshot_hash(list(sample_rows))
    mutated = [dict(sample_rows[0]), dict(sample_rows[1])]
    mutated[1]["visible"] = False
    check("树快照哈希：同样输入必得同哈希", hash_a == hash_b and len(hash_a) == 64)
    check("反向验证④：窗口树任一字段变化哈希必须变化",
          tree_snapshot_hash(mutated) != hash_a)

    # ---- D. 看门狗状态机四条路径（可注入 Fake，零真机依赖） ---------------- #
    normal, normal_fake = _fsm_with_fake(supported_snapshot(), "finished", True)
    check("看门狗[正常]：SUCCESS / 退出码 0 / 卸载平衡",
          normal.outcome == OUTCOME_SUCCESS and normal.exit_code == 0
          and normal.journal_balanced and normal.install_count == 1
          and normal.uninstall_count == 1 and normal.uninstall_attempted
          and normal.verify_ok)
    check("看门狗[正常]：零重启 Explorer / 零注入重试",
          (not normal.restart_explorer_attempted)
          and normal.retry_inject_count == 0
          and normal_fake.wait_calls == 1)

    timeout, _tf = _fsm_with_fake(supported_snapshot(), "timeout", True)
    check("看门狗[超时]：ABORTED / 非零码 / 仍走卸载且平衡",
          timeout.outcome == OUTCOME_ABORTED and timeout.exit_code != 0
          and timeout.journal_balanced and timeout.uninstall_attempted
          and (not timeout.restart_explorer_attempted))
    check("看门狗[超时]：写了 watchdog timeout 结构化日志",
          any(event.endswith(".watchdog") and kv.get("phase") == "timeout"
              for event, kv in timeout.events))

    crash, _cf = _fsm_with_fake(supported_snapshot(), "crash", True)
    check("看门狗[异常]：ABORTED / 非零码 / 仍走卸载且平衡",
          crash.outcome == OUTCOME_ABORTED and crash.exit_code != 0
          and crash.journal_balanced and crash.uninstall_attempted
          and (not crash.restart_explorer_attempted))

    mismatch_snap = supported_snapshot()
    mismatch_snap.edge = "Left"
    failsafe, fs_fake = _fsm_with_fake(mismatch_snap, "finished", True)
    failsafe_logs = [e for e, _kv in failsafe.events if e.endswith(".failsafe")]
    check("看门狗[FailSafe 不匹配]：禁用退出 / 零安装零卸载 / 退出码 0",
          failsafe.outcome == OUTCOME_DISABLED_FAILSAFE
          and failsafe.exit_code == 0
          and failsafe.install_count == 0 and failsafe.uninstall_count == 0
          and fs_fake.install_calls == 0 and fs_fake.uninstall_calls == 0)
    check("看门狗[FailSafe 不匹配]：恰好 1 条 WARN 且 hook_installed=false",
          len(failsafe_logs) == 1
          and failsafe.events[0][1].get("hook_installed") is False
          and failsafe.events[0][1].get("writes") == 0)

    verify_fail, _vf = _fsm_with_fake(supported_snapshot(), "finished", False)
    check("看门狗[补充]：verify 失败 -> ABORTED 非零码",
          verify_fail.outcome == OUTCOME_ABORTED and verify_fail.exit_code != 0
          and verify_fail.journal_balanced)

    good_run, _gr = _fsm_with_fake(supported_snapshot(), "finished", True)
    bad_run, _br = _fsm_with_fake(supported_snapshot(), "crash", True)
    check("反向验证⑤：把 wait 从 finished 改坏成 crash，outcome 必须由 success 变 aborted",
          good_run.outcome == OUTCOME_SUCCESS
          and bad_run.outcome == OUTCOME_ABORTED)

    # ---- D2. P3 / P7：日志故障与 install 失败下的卸载不变量 ------------------ #
    class ExplodingLogBackend(FakeBackend):
        """测试替身：日志后端抛异常，install / uninstall 仍正常记账。"""

        def log(self, event: str, **kv: Any) -> None:
            raise OSError("simulated log backend failure")

    class InstallRaisingBackend(FakeBackend):
        """测试替身：install 记账后抛异常（模拟真实后端第一行即记账）。"""

        def install(self, op: str) -> bool:
            self.journal.install(op)
            raise RuntimeError("simulated install failure")

    class InstallFalseBackend(FakeBackend):
        """测试替身：install 返回 False，wait 不应被调用（P7）。"""

        def install(self, op: str) -> bool:
            self.journal.install(op)
            self.install_calls += 1
            return False

    log_fake = ExplodingLogBackend(supported_snapshot(), wait_result="finished",
                                   verify_ok=True)
    log_result = WatchdogFSM(journal=log_fake.journal).run(log_fake,
                                                           DEFAULT_TIMEOUT_S)
    check("看门狗[P3]：日志后端抛异常仍必走卸载 + 卸载平衡 + 零重启零重试",
          log_result.journal_balanced and log_result.uninstall_attempted
          and log_fake.uninstall_calls == 1
          and (not log_result.restart_explorer_attempted)
          and log_result.retry_inject_count == 0)

    raise_fake = InstallRaisingBackend(supported_snapshot())
    raise_result = WatchdogFSM(journal=raise_fake.journal).run(raise_fake,
                                                               DEFAULT_TIMEOUT_S)
    check("看门狗[P3]：install 内部抛异常仍必走卸载 + 卸载平衡 + ABORTED",
          raise_result.journal_balanced and raise_result.uninstall_attempted
          and raise_fake.uninstall_calls == 1
          and raise_result.outcome == OUTCOME_ABORTED
          and raise_result.exit_code != 0)

    false_fake = InstallFalseBackend(supported_snapshot())
    false_result = WatchdogFSM(journal=false_fake.journal).run(false_fake,
                                                               DEFAULT_TIMEOUT_S)
    check("看门狗[P7]：install 返回 False 时不调 wait、仍必走卸载且 ABORTED",
          false_fake.wait_calls == 0
          and false_result.journal_balanced
          and false_fake.uninstall_calls == 1
          and false_result.outcome == OUTCOME_ABORTED)

    # ---- D3. P2 报告诚实化（纯函数，零真机） -------------------------------- #
    normal_fields = phase1_report_fields(normal, True)
    check("P2 报告诚实：正常路径 mode / inject / Q-a 均如实",
          normal_fields["mode"] == "phase1-readonly-inject"
          and normal_fields["inject"] is True
          and normal_fields["qa_status"] == "readonly_probe_executed")
    fs_fields = phase1_report_fields(failsafe, False)
    check("P2 反向①：FailSafe 零注入时 inject=False / mode=disabled / Q-a=not_executed",
          fs_fields["inject"] is False
          and fs_fields["mode"] == "phase1-failsafe-disabled"
          and fs_fields["qa_status"] == "not_executed")
    to_fields = phase1_report_fields(timeout, False)
    check("P2 反向②：超时 aborted 时 mode=aborted 且 Q-a 不谎报已执行",
          to_fields["mode"] == "phase1-aborted"
          and to_fields["qa_status"] == "incomplete")
    check("P2 反向③：PHASE_NORMAL 但结果文件未生成时 Q-a 必须 incomplete",
          phase1_report_fields(normal, False)["qa_status"] == "incomplete")

    # ---- E. 卸载完整性（每笔 install 都有对应 uninstall） ------------------- #
    journal = InstallJournal()
    journal.install("readonly-dll")
    check("卸载完整性：只 install 未 uninstall 时不平衡", not journal.is_balanced())
    journal.uninstall("readonly-dll")
    check("卸载完整性：install 与 uninstall 一一对应时平衡", journal.is_balanced())
    broken_fake = FakeBackend(supported_snapshot(), skip_uninstall_journal=True)
    broken_fsm = WatchdogFSM(journal=broken_fake.journal)
    raised = False
    broken_diag = ""
    try:
        broken_fsm.run(broken_fake, DEFAULT_TIMEOUT_S)
    except AssertionError as exc:
        raised = True
        broken_diag = str(exc)
    check("反向验证⑥：故意漏掉一笔 uninstall 必须让完整性断言变红",
          raised and "uninstall incomplete" in broken_diag)

    # ---- F. 红线源码静态断言 ----------------------------------------------- #
    rules = build_redline_rules()
    check("红线规则表完备性：11 个必查 token 齐备且条数 >= 36",
          assert_required_redline_tokens(rules))
    reduced_all = []
    for required_token in required_redline_tokens():
        reduced = [rule for rule in rules if rule[0] != required_token]
        reduced_all.append(assert_required_redline_tokens(reduced))
    check("反向验证⑫：删掉任一必查 token 后完备性断言必须变红",
          not any(reduced_all))
    paths = _redline_scan_paths()
    has_main_py = any(p.lower().endswith(".py") for p in paths)
    has_dll_c = any(p.lower().endswith(".c") for p in paths)
    check("红线扫描覆盖：主程序 .py 与只读 DLL 源码 .c 都在扫描集合内",
          len(paths) >= 2 and has_main_py and has_dll_c)
    clean_hits = assert_source_clean(paths, rules)
    check("红线静态断言：全部实验源码零禁用 API（零 hook / 零窗口过程替换 / "
          "零颜色写入 / 零注册表写入 / 零强杀 / 零资源替换）",
          clean_hits == [])

    bad_sample = "\n".join(token + "(arg);" for token, _desc in rules)
    bad_hits = scan_redlines(bad_sample, rules)
    check("反向验证⑦：把每条禁用 API 注入坏样本，必须逐条被检出",
          len(bad_hits) == len(rules))

    target = rules[0][0]
    broken_rules = [rule for rule in rules if rule[0] != target]
    single_sample = "call " + target + "();"
    check("反向验证⑧：删掉某条红线规则后对应 token 不再被检出",
          scan_redlines(single_sample, rules) != []
          and scan_redlines(single_sample, broken_rules) == [])

    sources = collect_source_text(paths)
    combined = "\n".join(sources.values())
    injected = combined + "\n/* " + target + " */\n"
    check("反向验证⑨：干净源码注入一个禁用 token 后必须被检出",
          scan_redlines(combined, rules) == []
          and len(scan_redlines(injected, rules)) > 0)

    # ---- G. 接线 / 诚实性 源码断言（含反向） -------------------------------- #
    src_text = sources[_SCRIPT_PATH]
    phase1_slice = _function_slice(src_text, "def run_phase1(")
    uia_slice = _function_slice(src_text, "def uia_capability_probe(")
    check("接线静态断言：阶段 1 经看门狗状态机驱动真实后端",
          assert_phase1_wiring(phase1_slice))
    cut = phase1_slice.replace("fsm.run(backend, timeout_s)", "pass  # wiring cut")
    check("反向验证⑩：剪断 fsm.run 接线后接线断言必须变红",
          not assert_phase1_wiring(cut))
    check("诚实性静态断言：UIA 未完成遍历时如实标注、节点为空、不谎报可写入口",
          assert_uia_honesty(uia_slice))
    dishonest = uia_slice.replace('"uia_traversal_completed": False',
                                  '"uia_traversal_completed": True')
    check("反向验证⑪：把 UIA 未完成标注谎报为完成时断言必须变红",
          not assert_uia_honesty(dishonest))

    # ---- H. 日志替身（只记内存，不写文件） --------------------------------- #
    recorder = RecordingLogger()
    recorder.emit("g9.experiment.selftest", phase="selftest", hresult=0)
    line = recorder.line("g9.experiment.selftest", phase="selftest", hresult=0)
    check("日志替身：只记内存且 line 为无空格 key=value",
          len(recorder.records) == 1 and "event=g9.experiment.selftest" in line
          and " " in line and "hresult=0" in line)

    # ---- I. P4 编译记录完整性（纯函数，零文件写入） ------------------------- #
    good_record = {"compiled": True, "sha256": "a" * 64,
                   "source_sha256": "b" * 64}
    check("P4 编译记录：compiled + 产物/源码双哈希一致 -> 可执行",
          verify_build_record(good_record, True, "a" * 64,
                              True, "b" * 64)[0])
    tampered = dict(good_record)
    tampered["sha256"] = "deadbeef" * 8
    bad_dll = verify_build_record(tampered, True, "a" * 64, True, "b" * 64)
    check("P4 反向①：产物 sha256 被篡改 -> 不可执行（dll_hash_mismatch）",
          (not bad_dll[0]) and "dll_hash_mismatch" in bad_dll[1])
    no_src = {"compiled": True, "sha256": "a" * 64}
    missing_src = verify_build_record(no_src, True, "a" * 64, True, "b" * 64)
    check("P4 反向②：缺 source_sha256 -> 按安全方向判不可用（不静默通过）",
          (not missing_src[0]) and "source_hash_missing" in missing_src[1])
    bad_src = dict(good_record)
    bad_src["source_sha256"] = "c" * 64
    src_mismatch = verify_build_record(bad_src, True, "a" * 64, True, "b" * 64)
    check("P4 反向③：源码 sha256 不一致 -> 不可执行（source_hash_mismatch）",
          (not src_mismatch[0]) and "source_hash_mismatch" in src_mismatch[1])
    empty_hash = dict(good_record)
    empty_hash["sha256"] = ""
    check("P4 反向④：产物 sha256 置空 -> 不可执行（dll_hash_missing）",
          (not verify_build_record(empty_hash, True, "a" * 64,
                                   True, "b" * 64)[0]))

    # ---- J. P6 超时边界（nan / inf / 0 / 负数一律拒绝） --------------------- #
    check("P6 超时校验：15 / 0.5 / 1e9 为有效有限正数",
          all(validate_timeout(v) for v in (15.0, 0.5, 1e9)))
    check("P6 超时校验：0 / -3 / nan / inf / -inf 全拒绝",
          not any(validate_timeout(v) for v in
                  (0, -3, float("nan"), float("inf"), float("-inf"))))

    # ---- Z. 元断言：证明 self-test 全程零系统 API 触碰 ---------------------- #
    check("元断言：self-test 全程零系统 API 触碰（未加载 Win32 / COM、未注入、"
          "未写注册表、未写颜色）", _SYSTEM_API_TOUCHED is False)

    failed = [name for name, ok in checks if not ok]
    print("G9 组①扩展 受控可行性实验自检：%d 项，通过 %d，失败 %d" % (
        len(checks), len(checks) - len(failed), len(failed)))
    for name, ok in checks:
        print("  [%s] %s" % ("PASS" if ok else "FAIL", name))
    if failed:
        print("\n反向验证说明：判定表 / 规则表 / 接线 / 卸载被改坏时，"
              "对应断言必须变红，否则测试锁不住接线（G8 血泪教训）。")
        return 1
    print("\n结论：真值表 / 反向验证 / 看门狗状态机 / 卸载完整性 / 红线静态断言 / "
          "编译记录完整性 / 超时边界均通过（未接触任何系统 API、未注入、"
          "未写注册表、未写颜色）。")
    return 0


def validate_timeout(value: float) -> bool:
    """纯函数：超时必须是有限正数（P6：拦 0 / 负数 / nan / inf）。"""
    try:
        number = float(value)
    except (TypeError, ValueError):
        return False
    return math.isfinite(number) and number > 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    """CLI 入口：--self-test / 默认阶段 0 / --allow-inject 阶段 1。"""
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    parser = argparse.ArgumentParser(
        description="G9 组①扩展 受控可行性实验（零注入优先，分级执行）")
    parser.add_argument("--self-test", action="store_true",
                        help="纯逻辑自检，不碰任何系统 API")
    parser.add_argument("--allow-inject", action="store_true",
                        help="显式授权阶段 1 只读注入（仅主理人真机执行）")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S,
                        help="看门狗硬超时秒数（默认 15）")
    parser.add_argument("--log-file", default=None,
                        help="可选：追加写结构化日志文件（默认仅 stdout）")
    parser.add_argument("--out-json", default=None,
                        help="可选：写完整结构化报告 JSON（默认仅 stdout）")
    parser.add_argument("--env-json", default=None,
                        help="可选：主理人已核对的环境快照（operator-supplied）")
    parser.add_argument("--result-file", default=_RESULT_PATH,
                        help="阶段 1 探针 DLL 回写结果路径")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()
    if not validate_timeout(args.timeout):
        print("错误：--timeout 必须为有限正数（0 / 负数 / nan / inf 一律拒绝）",
              file=sys.stderr)
        return 2

    env_override: Optional[EnvSnapshot] = None
    if args.env_json:
        try:
            with open(args.env_json, "r", encoding="utf-8") as handle:
                env_override = _env_from_json(json.load(handle))
        except Exception as exc:
            print("错误：--env-json 读取失败：%s" % type(exc).__name__,
                  file=sys.stderr)
            return 2

    logger = StructuredLogger(log_file=args.log_file)
    try:
        if args.allow_inject:
            code, report = run_phase1(logger, args.timeout, env_override,
                                     args.result_file)
        else:
            report = run_phase0_readonly(logger, env_override)
            code = 0
        print(json.dumps(report, ensure_ascii=False, indent=2))
        if args.out_json:
            with open(args.out_json, "w", encoding="utf-8") as handle:
                json.dump(report, handle, ensure_ascii=False, indent=2)
        return code
    except Exception as exc:  # pragma: no cover - 真机兜底
        logger.emit(EVENT_PREFIX + ".end", phase="error", outcome="aborted",
                    reason=type(exc).__name__, exit_code=1,
                    restart_explorer=False, retry_inject=0,
                    note="实验异常：只记录并退出非零码，不自动重启 Explorer、"
                         "不自动重试注入。")
        print("实验异常（已记录，不自动重启 Explorer / 不重试注入）：%s"
              % type(exc).__name__, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())