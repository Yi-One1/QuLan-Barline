# 曲栏 Barline — M5 交付前终验报告(v1.0.0 / 84fe061)

- **验收人**:Edward(software-qa-engineer)
- **验收对象**:main **84fe061**(= 5b425ac 版本资源 1.0.0 + 84fe061 README/LICENSE);交付物 `deliverables/曲栏_Barline_v1.0.0.exe`(955 KB)+ `.zip`(341 KB)
- **验收方式**:独立 worktree(基于 84fe061),全部构建 / 真机 / 便携包 / 远端核对自跑,不采信自述
- **结论**:**通过 —— 建议发布 v1.0.0 Release**(无缺陷、无阻塞项)

---

## 1. 静态终态 ✅

```
cargo test (build.ps1):        222 passed / 0 failed / 1 ignored,退出码 0
cargo clippy -D warnings:      退出码 0,零告警
cargo build --profile release: 退出码 0,零 warning,barline.exe = 954.5 KB(crate 已为 barline v1.0.0)
```

## 2. exe 版本属性独立复核 ✅(worktree 重建的 release exe)

| 字段 | 值 |
| --- | --- |
| FileVersion / ProductVersion | **1.0.0** / **1.0.0** |
| ProductName | 曲栏 Barline(中文正常) |
| FileDescription | 曲栏 Barline — Windows 任务栏媒体播控小组件(中文正常) |
| CompanyName / LegalCopyright | The Barline Authors / MIT License. The Barline Authors. |
| OriginalFilename | barline.exe |

交付包内 `曲栏_Barline_v1.0.0.exe`:FileVersion/ProductVersion=1.0.0、ProductName 中文正常、955.0 KB —— 与 worktree 构建产物一致。**rc UTF-8 BOM + `#pragma code_page(65001)` 修复经独立复核有效。**

## 3. 真机抽查 ✅

- **关于页版本 = 1.0.0**(`m4-qa4-10-about-version.bmp`:版本 / 日志目录 / 歌词来源 lrclib.net / 开源许可 MIT License / 致谢,无乱码)——v0.0.x 时代的「关于页版本 0.0.1」观察项就此关闭
- **标题栏图标正常**:蓝色圆角「曲」字标出现在设置窗口标题栏,窗口圆角保持(`m4-qa4-11-window-icon.bmp`)
- **托盘 / 快捷键 / 单实例 / 退出**(`m4-qa2-01/02` 沿用):托盘三项菜单 + 勾选态翻转 ✅;6 组全局快捷键 6/6 触发 ✅;单实例 41 ms ✅;托盘「退出」日志 81 ms ✅

## 4. 便携包端到端 ✅

`曲栏_Barline_v1.0.0.zip` 解压(内容 = exe + README.md + LICENSE)到项目内临时目录直跑:

1. 进程启动,日志「曲栏 Barline v1.0.0 启动」;托盘图标 ✅、小组件可见 ✅
2. 二次启动 → 单实例唤起 → 设置页打开且 WebView2 就绪 ✅
3. 托盘「退出」→ 日志「托盘：退出」→「进程退出,退出码 0」(内部 24 ms)✅
4. 测试临时目录已清理,未污染仓库与 %LOCALAPPDATA%(配置文件验收后已还原)

## 5. 远端一致性 ✅

```
git ls-remote origin main  →  84fe0612b5278e5aa013a48e7130d6362aa2190d
本地 main HEAD             →  84fe061(同一哈希 ⇒ README/LICENSE 等全部内容一致)
```

## 6. 文档抽查 ✅

- **README**:默认快捷键表与实际 6 组完全一致(Ctrl+Alt+Shift+P/←/→/L/M/H);构建说明与真实约束吻合(必须经 `scripts/build.ps1`、Git Bash `link.exe` 抢占问题、BuildTools/SDK 依赖、单文件产出)——无误导
- **LICENSE**:标准 MIT License 文本

## 7. 截图索引(已入 `docs/screenshots/`)

| 文件 | 内容 |
| --- | --- |
| m4-qa4-10-about-version.bmp | 关于分区:版本 1.0.0 / MIT / 致谢 |
| m4-qa4-11-window-icon.bmp | 整窗:标题栏图标 + 圆角 + Mica |

## 8. 发布建议

**可以发布 v1.0.0 GitHub Release。** 建议随 Release 附上 `deliverables/曲栏_Barline_v1.0.0.zip`(内含 exe + README + LICENSE,自包含),Release Notes 可直接引用 README 功能清单;历史 QA 报告(QA-M2 / QA-M4 / QA-M3P2 / 本报告)均在 docs/ 可供查阅。
