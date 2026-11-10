# G9 阶段 1 · 最小只读探针 DLL

> 本目录只放**阶段 1（只读注入，默认关闭）**的最小探针源码、编译脚本与产物。
> 主程序在 `scripts/g9_inject_probe.py`；本目录**不进 `src/`、不改 `Cargo.toml`、
> 不改根目录 `build.ps1`、不出包、不升版本、不写 `docs/` 报告**。

## 1. 文件清单

| 文件 | 性质 | 说明 |
|---|---|---|
| `probe.c` | 源码（新增） | 最小只读 DLL。全 ASCII 英文注释，MSVC `cl.exe /LD` 可编。 |
| `build.ps1` | 编译脚本（新增） | 探测 `cl.exe` → 编译 → 写 `build-result.json`。只用 PowerShell 原生能力，不调 `cmd.exe`、不用 `Add-Type`、不用 `reg.exe`、不跑裸 cargo。 |
| `build-result.json` | 编译产物记录（脚本生成） | `compiled` / `reason` / `toolchain` / `sha256`（DLL）/ `source_sha256`（probe.c）/ `stdout_tail`。主程序 `_dll_ready()` 同时校验产物与源码哈希，任一不一致或缺字段按安全方向判不可用，**绝不静默通过**。 |
| `g9_readonly_probe.dll` | 编译产物（若编译成功） | 目标文件名与主程序 `_DLL_PATH` 一致。 |
| `inject_result.txt` | 真机回写结果（若阶段 1 执行） | DLL 导出函数把进程内只读枚举结果以 UTF-8 文本写到此文件。 |
| `tests/test_doubles.py` | 测试替身（可独立运行） | 复用主程序公共符号跑看门狗状态机 / 卸载完整性 / 红线断言，零系统 API。 |

## 2. 探针 DLL 的硬约束（逐条对应主理人 spec）

- **DllMain 什么都不做**：函数体只有 `return TRUE;`，不初始化 COM、不加载库、不创建线程、不碰注册表、不写系统状态。
- **只导出一个函数** `G9ReadOnlyProbe(LPVOID)`，供远程线程作为入口调用；函数体只做只读枚举：
  1. 进程内 `Shell_TrayWnd` 窗口树（类名 / 标题 / 可见性 / 矩形 / pid）；
  2. 运行时模块是否已映射（`Windows.UI.Xaml.dll` / `UIAutomationCore.dll` / `dcomp.dll` / `dwmapi.dll` / `Windows.UI.dll`）——**仅作为间接可达性信号**，模块存在不等于接口可用；
  3. 当前进程 pid。
  - 结果通过 `CreateFileW` + `WriteFile` 回写 UTF-8 文本文件（路径由主程序写进目标进程内存后传入）。
- **绝对不做**（主程序 `--self-test` 的红线源码静态断言会逐条核对本目录全部源码）：
  - 不装任何消息 / 事件钩子，不替换窗口过程，不做窗口子类化；
  - 不写任何颜色，不改合成 / DWM 窗口属性，不改分层窗口 alpha；
  - 不写注册表，不改全局主题 / 强调色，不替换系统资源；
  - 不强制结束进程，不重启 Explorer，不改窗口父级，不改其它窗口 / 其它进程；
  - 不调用任何会改变系统状态的设置类接口。
- **正因为没有任何窗口过程指向本 DLL**，卸载路径才敢走库释放：远程线程返回 → `FreeLibrary` → 释放远程内存 → 校验 Explorer 存活 / 窗口树哈希一致 / 无残留与新增模块。这与架构文档 §2.3 第 229 行「WndProc 指向 DLL 时释放会崩 Explorer」的风险前提不同，本实验刻意不制造该前提。

## 3. 编译（主理人 / 带 MSVC 环境时执行）

```powershell
powershell -ExecutionPolicy Bypass -File scripts/g9_inject_probe/build.ps1
```

- 脚本三层查找 `cl.exe`：PATH → `vswhere.exe` → 只读扫描 VS / BuildTools 根目录（本机是 `2022\BuildTools` 布局，`vswhere` 返回空时靠第三层兜底）。
- **不 source Developer 的 `.bat`**（`cmd.exe` 被禁）：脚本从 `cl.exe` 所在目录推导 MSVC `include` / `lib\x64`，再从 Windows Kits 10 目录选最新 SDK 推导 `ucrt` / `um` / `shared` / `winrt`，仅在**当前 PowerShell 进程作用域**设置 `INCLUDE` / `LIB` / `PATH`；不改系统、不写注册表、不用 `Add-Type`、不跑裸 cargo。
- 任一步失败都如实写 `compiled=false` 与失败输出片段，**绝不假装编译成功**。
- 工程师回合只负责「源码 + 编译脚本 + 如实记录编译结果」；**不把 DLL 加载进 Explorer**。

## 4. 阶段 1 真机执行（仅主理人，代码与自测全绿后）

```powershell
python scripts/g9_inject_probe.py --self-test
python scripts/g9_inject_probe.py                       # 阶段 0：零注入只读
python scripts/g9_inject_probe.py --allow-inject --timeout 15 \
    --log-file <path> --out-json <path> --result-file <path>
```

- 若 `build-result.json` 为 `compiled=false`、DLL / `probe.c` 不存在、或产物 / 源码 sha256 与记录不一致（含缺 `sha256` / `source_sha256`），主程序写 `g9.experiment.blocked` 并以退出码 2 结束，**如实标注「阶段 1 不可执行」，不假装跑过**。
- `--timeout` 必须是有限正数；`0` / 负数 / `nan` / `inf` / 非数字一律 rc=2，不会进入真机路径。
- 环境 fail-safe 不匹配时只写 1 条 WARN、不注入、正常禁用退出；主理人可用 `--env-json` 传入**已核对**的环境快照（日志记 `env_source=operator-supplied`，不伪造）。
- 看门狗默认 15s 硬超时；超时 / 异常 / 崩溃都触发卸载流程与结构化日志 `g9.experiment.*`。
- **任何失败都不自动重启 Explorer、不自动重试注入**，只记录并退出非零码。
- 超时路径为保护 Explorer 会**跳过库释放**（远程线程可能仍在 DLL 内执行，此时释放库会让 Explorer 崩溃），如实记录 `inactive DLL 可能残留到 Explorer 自然重启` 并判非零码；这属于 Q-b 的**残余风险**，不伪装成「卸载成功」。

## 5. 如实声明

- 工程师回合**未执行任何真机注入**、未加载本 DLL 到 Explorer、未改系统、未写注册表、未写颜色、未启动 GUI 程序、未截图、未提交 git。
- 本目录是否编译成功，以 `build-result.json` 与汇报为准；若未编译，阶段 1 **不可执行**。
- 阶段 0 的 UIA 完整自动化树遍历在本项目「零第三方依赖 + 禁 `Add-Type`」约束下未实现，主程序会如实标注 `uia_traversal_completed=false` 与原因，并让节点列表保持为空——**空列表表示未执行遍历，不表示目标元素不存在**，绝不伪造 UIA 结果。

## 6. 工程师回合编译实测（如实，均为【观察】）

| 项 | 实测结果 |
|---|---|
| 工具链 | MSVC BuildTools 2022，`cl.exe` 14.44.35207 `Hostx64\x64`；Windows Kits 10 SDK `10.0.26100.0`；`rustc 1.99.1` 亦在（host `x86_64-pc-windows-msvc`），但本组选 `cl.exe` 编 C 源码，未新增 Rust 版探针 |
| `build-result.json` | `compiled=true`、`reason=compiled`、`sha256`（DLL）= `67b54d7854139fb57c5814c7fc3b0139ead9a7717362c4de6ca72ae337b53652`、`source_sha256`（probe.c）= `e25487b43eaf7bc5391a66b7c5ccdba6b0eaf42f082c316b114df8aed77dde89` |
| 产物 | `g9_readonly_probe.dll`，sha256 `67b54d7854139fb57c5814c7fc3b0139ead9a7717362c4de6ca72ae337b53652`（P4 修复后重编译，故与首轮 `67db67ff…` 不同） |
| PE 只读核对 | `Machine=0x8664`（x64）、`Magic=0x20B`（PE32+）；导出表**恰好 1 个符号** `G9ReadOnlyProbe`，与「只导出一个函数」一致（纯 Python 解析 PE 导出表，**未加载 / 未执行该 DLL**） |
| 阶段 1 前置 | 主程序 `_dll_ready()` 返回 `(True, 'dll_present_verified')`（P4 双哈希校验通过）→ 阶段 1 **具备可执行前置条件** |
| 自测 | `python scripts/g9_inject_probe.py --self-test` → **59 项 / 通过 59 / 失败 0 / 退出码 0**（QA 第一轮 43 项全部保留，新增 16 项）；`tests/test_doubles.py` → **11 项 / 通过 11 / 失败 0 / 退出码 0**；`py_compile` 通过 |
| **未执行** | **阶段 0 真机只读探测、阶段 1 真机注入、把 DLL 加载进 Explorer、卸载流程，工程师回合一律未执行**；真机实验由主理人在代码与自测全绿后亲自执行 |

编译同时产生 `probe.obj` / `g9_readonly_probe.lib` / `g9_readonly_probe.exp`，均为编译中间产物，同目录存放，不进 `src/`、不改 `Cargo.toml`、不改根目录 `build.ps1`、不提交 git。

> 注意：「已编译」只证明**阶段 1 可执行**，**不等于** Q-b 已被回答。Q-b 的完整卸载结论只能由主理人真机跑完阶段 1 后的 `g9.experiment.verify` / `uninstall` 结构化日志产出；本目录与工程师回合不提供任何真机注入观察。

## 7. 红线静态断言的能力边界（P5，如实标注）

QA 第一轮验证指出：`scan_redlines` 是 `text.lower().count(token.lower())` 的**纯文本子串匹配**，只能覆盖源码里的连续字面量。本组**明确声明以下边界，不夸大断言能力**：

- **不宣称能拦 7 类绕过**：① 字符串分片拼接（例 `"SetWindow" + "Subclass"`）② `getattr` / 反射式动态调用 ③ 十六进制 / Unicode 转义（例 `\x53` 开头）④ 零宽字符插入 ⑤ 全角字符替换 ⑥ 下划线 / 注释分隔 ⑦ 大小写 / 编码变体。这 7 类 QA 实测均为 `detected=False`。
- **与设计同源的固有边界**：主程序 docstring 纪律 4 为了让红线扫描器能安全地扫描自身源码，本身就采用「分片拼接」定义 token 表；因此「分片拼接」这一类绕过不可能靠同一个文本扫描器自洽地拦住，属设计取舍而非遗漏。P1 新增的 `assert_required_redline_tokens` 锁的是**规则表绝对内容**，不是绕过检测能力。
- **扫描集合有限**：仅 `g9_inject_probe.py`、`probe.c`、`build.ps1`、`tests/*.py` 四个**源码文本**；**不扫描 DLL 二进制 / PE 导入表**。
- **可选加固未做**：QA 允许补一条「纯标准库 PE 导入表静态解析（不加载 / 不执行 DLL）」，本轮**未做**，理由：范围控制、不扩大实验范围、不引入第三方依赖；且导入表解析同样不能证明运行时行为，不能替代人工 review。
- **断言定位**：红线静态断言的价值是「锁接线 + 防手滑 + 反向验证能变红」，**不是**完备的恶意代码检测。真机红线仍以人工 review + `g9.experiment.*` 结构化日志为主证据（PRD §7 证据优先级）。
- **P9 边界由主理人裁定**：实验工具 `scripts/` 使用远程线程 / 进程内存写入 / 虚拟内存分配，与 ARCH §4.9 V13「产品源码红线」字面存在张力；主理人已裁定属实验工具、不进 `src/`，不构成产品红线放宽，本组不因此删除这些调用。
