#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""G9 阶段 1 测试替身：可独立运行，复用主程序公共符号，零系统 API。

用法：
    python scripts/g9_inject_probe/tests/test_doubles.py

只 import 主程序的纯逻辑符号；不构造真实后端、不加载任何系统 DLL、
不接触 Win32 / COM / 注册表 / 注入。与主程序 --self-test 同源，
避免「双份实现漂移」。
"""

from __future__ import annotations

import importlib.util
import os
import sys
from typing import Any, List, Tuple


def _load_main():
    """按明确路径加载主程序 .py（避免与同名目录产生 import 歧义）。"""
    here = os.path.dirname(os.path.abspath(__file__))
    main_path = os.path.abspath(
        os.path.join(here, "..", "..", "g9_inject_probe.py"))
    spec = importlib.util.spec_from_file_location("g9_inject_probe_main",
                                                  main_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot locate g9_inject_probe.py")
    module = importlib.util.module_from_spec(spec)
    # dataclass 解析注解时需要模块已登记到 sys.modules
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def run() -> int:
    """跑四条状态机路径 + 卸载完整性 + 红线断言 + 元断言。"""
    g = _load_main()
    checks: List[Tuple[str, bool]] = []

    def check(name: str, cond: Any) -> None:
        checks.append((name, bool(cond)))

    # 看门狗状态机：正常 / 超时 / 异常（三条都必走卸载）
    cases = (
        ("正常", "finished", g.OUTCOME_SUCCESS, 0),
        ("超时", "timeout", g.OUTCOME_ABORTED, 1),
        ("异常", "crash", g.OUTCOME_ABORTED, 1),
    )
    for label, wait_result, outcome, code in cases:
        fake = g.FakeBackend(g.supported_snapshot(), wait_result=wait_result,
                             verify_ok=True)
        fsm = g.WatchdogFSM(journal=fake.journal)
        result = fsm.run(fake, g.DEFAULT_TIMEOUT_S)
        check("状态机[%s]：outcome / 退出码 / 卸载平衡 / 零重启零重试" % label,
              result.outcome == outcome
              and (result.exit_code != 0) == (code != 0)
              and result.journal_balanced
              and result.uninstall_attempted
              and (not result.restart_explorer_attempted)
              and result.retry_inject_count == 0)

    # 第四条路径：FailSafe 不匹配 —— 零安装零卸载、正常禁用退出
    mismatch = g.supported_snapshot()
    mismatch.monitor_count = 2
    fake_fs = g.FakeBackend(mismatch)
    fs = g.WatchdogFSM(journal=fake_fs.journal).run(fake_fs,
                                                    g.DEFAULT_TIMEOUT_S)
    check("状态机[FailSafe 不匹配]：零安装零卸载 + 正常禁用退出",
          fs.outcome == g.OUTCOME_DISABLED_FAILSAFE
          and fs.exit_code == 0
          and fs.install_count == 0 and fs.uninstall_count == 0)

    # 卸载完整性 + 反向验证
    journal = g.InstallJournal()
    journal.install("readonly-dll")
    check("卸载完整性：只 install 时不平衡", not journal.is_balanced())
    journal.uninstall("readonly-dll")
    check("卸载完整性：一一对应时平衡", journal.is_balanced())
    broken = g.FakeBackend(g.supported_snapshot(), skip_uninstall_journal=True)
    raised = False
    try:
        g.WatchdogFSM(journal=broken.journal).run(broken, g.DEFAULT_TIMEOUT_S)
    except AssertionError:
        raised = True
    check("反向验证：漏掉 uninstall 必须让完整性断言变红", raised)

    # 红线源码静态断言 + 反向验证
    rules = g.build_redline_rules()
    paths = g._redline_scan_paths()
    check("红线扫描覆盖 >=3 个源码文件", len(paths) >= 3)
    check("红线静态断言：全部源码零禁用 API",
          g.assert_source_clean(paths, rules) == [])
    bad = "\n".join(token + "(arg);" for token, _desc in rules)
    check("反向验证：坏样本每条红线都被检出",
          len(g.scan_redlines(bad, rules)) == len(rules))

    # 元断言：本文件运行全程零系统 API 触碰
    check("元断言：零系统 API 触碰", g._SYSTEM_API_TOUCHED is False)

    failed = [name for name, ok in checks if not ok]
    print("G9 阶段 1 测试替身：%d 项，通过 %d，失败 %d" % (
        len(checks), len(checks) - len(failed), len(failed)))
    for name, ok in checks:
        print("  [%s] %s" % ("PASS" if ok else "FAIL", name))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(run())
