# 开发者说明

本文面向需要从源码运行、构建、测试或发布 AutoBiubiu 的开发者。普通用户的下载与使用见 [README](../README.md)。

## 从源码启动

需要 Windows 10/11、已安装并登录的 biubiu PC 客户端，以及以下开发环境：

| 依赖 | 要求 |
| --- | --- |
| Node.js | 22.12.0 或更新版本，包含 npm |
| Rust | 1.85 或更新版本，MSVC 工具链；建议使用当前 stable |
| Visual Studio Build Tools | C++ 桌面开发工作负载与 Windows SDK |
| Microsoft Edge WebView2 | 桌面应用运行时 |

```powershell
git clone https://github.com/a08381/AutoBiubiu.git
Set-Location AutoBiubiu
npm ci
npm start
```

`npm start` 启动完整 Tauri 开发版；`npm run dev` 仅启动前端预览，没有桌面 IPC，不能操作客户端。

也可双击 [Start-AutoBiubiu.cmd](../Start-AutoBiubiu.cmd)：优先启动 debug/release 中修改时间较新的 EXE，时间相同优先 release；尚无 EXE 时回退到开发模式。直接运行已构建的 EXE 不需要 Node.js 或 Rust。

发行版启动时会请求 UAC 管理员确认；开发版仅在启动客户端需要提权时请求。客户端识别与连接由 Rust 和 Windows API 实现，不需要外置连接脚本。

## 构建

```powershell
npm run build                 # 类型检查并构建前端
npm test                      # 前端、Rust、便携包与自动发布测试
npm run pack                  # 构建 release EXE
npm run dist                  # 构建 release EXE 并生成便携 ZIP
npm run dist:nsis             # 构建 NSIS 安装包
```

| 命令 | 产物或用途 |
| --- | --- |
| `npm run check` | Svelte / TypeScript 检查 |
| `npm run test:frontend` | Vitest 前端测试 |
| `npm run test:rust` | Cargo 后端测试 |
| `npm run test:packaging` | 便携包结构、版本、失败保护与 Windows ZIP 校验测试 |
| `npm run test:release` | 版本检测、提交区间、描述生成及发布失败恢复测试 |
| `npm run build` | 前端产物 `build\`，不是独立桌面应用 |
| `npm run pack` | `src-tauri\target\release\autobiubiu.exe` |
| `npm run dist` | `src-tauri\target\release\AutoBiubiu_<version>_portable.zip` |
| `npm run dist:nsis` | `src-tauri\target\release\bundle\nsis\` 下的安装包 |
| `npm run build:icons` | 重新生成受版本控制的 PNG / ICO 图标 |

`npm run dist` 需要在 Windows 上运行，使用系统自带的 Windows PowerShell / .NET 压缩，无需额外安装 ZIP 工具。它先构建当前源码的 release EXE，再生成仅含 `AutoBiubiu/AutoBiubiu.exe` 的 ZIP，校验解压内容与构建 EXE 的 SHA-256 一致后替换同名旧包；构建或压缩失败时保留旧包。版本号取自 `tauri.conf.json`，并核对其与 `package.json` 一致。设置 `CARGO_TARGET_DIR` 或 `CARGO_BUILD_TARGET` 时，产物放在对应 Cargo 输出目录的 `release` 下。

便携包不包含个人 `data`、配置、日志或 WebView2 运行时，详见 [记录与配置文件](data.md)。

NSIS 安装范围为当前 Windows 用户，必要时下载 WebView2 bootstrapper；目前产物未配置代码签名。依赖版本由 [package-lock.json](../package-lock.json) 与 [Cargo.lock](../src-tauri/Cargo.lock) 固定，二者都应提交。

## 测试

- 前端测试位于 [test/frontend.test.ts](../test/frontend.test.ts)，覆盖活动筛选、任务配置、时间显示和输入校验。
- Rust 测试位于各模块的测试区，覆盖协议归一化、任务计时、状态持久化、迁移和结果不明处理。
- [启动脚本测试](../test/launch.test.ps1)和[旧配置归档测试](../test/cleanup-profile.test.ps1)使用临时目录，不操作真实业务数据。

远端协议变化时，用去除账号信息的最小合成数据补充回归测试。

### 桌面冒烟测试

需要已构建的 EXE。先关闭已有 AutoBiubiu 实例；测试 release 版本时使用管理员终端：

```powershell
npm run test:desktop
# 或指定另一个构建：
node scripts\smoke-desktop.js C:\path\to\autobiubiu.exe
```

[桌面冒烟测试](../scripts/smoke-desktop.js)使用显式 `--smoke-test` 隔离目录，检查真实 IPC、导航、主题、搜索、勾选、布局、设置保存、托盘、单实例和退出保存，同时验证关机横幅倒计时、跨页及刷新保留、取消失败提示与取消成功。关机状态与取消结果均为隔离夹具，不调用 `shutdown.exe`；不连接 biubiu，也不执行加速、领取或抽奖。报告与截图写入新建的 `data\tauri-smoke-*\`，不会提交到仓库。

### 辅助脚本测试

```powershell
powershell.exe -NoProfile -File test\launch.test.ps1
pwsh -NoProfile -File test\cleanup-profile.test.ps1
```

旧 Electron 配置归档工具及其测试需要 PowerShell 7.2+，应用本身不需要它。工具默认只预览，使用 `-Apply` 才归档；保留业务数据与当前 Tauri WebView 配置，使用说明见 [Clean-AutoBiubiuProfile.ps1](../scripts/Clean-AutoBiubiuProfile.ps1)。

## GitHub 自动发布

[Release portable 工作流](../.github/workflows/release.yml) 在推送到 `main` 时检查当前版本，也支持在 Actions 页面手动运行。当前版本已有正式或预发布 Release 时跳过；正式版低于已发布正式版本、预发布版低于已发布版本时同样跳过。未来版本的 beta 不会阻止当前正式版的补丁发布。版本标签沿用仓库现有的 `0.3.2` 格式，同时兼容历史 `v0.3.2` 标签。

发布新版本时，同步修改 [package.json](../package.json)、[package-lock.json](../package-lock.json) 中的根包版本、[tauri.conf.json](../src-tauri/tauri.conf.json)、[Cargo.toml](../src-tauri/Cargo.toml) 与 [Cargo.lock](../src-tauri/Cargo.lock) 中的 `autobiubiu` 版本，然后提交并推送。不需要手动创建 tag；工作流将标签绑定到本次 push 的确切提交。如果同版本标签已指向其他提交，工作流会报错，避免源码与附件不一致。

新版本通过前端、Rust、打包与发布脚本测试后，运行 `npm run dist`。发布脚本先创建草稿，上传并校验 `AutoBiubiu_<version>_portable.zip`，再公开 Release；失败时保留草稿，可重跑原提交的工作流继续上传。SemVer 预发布版本（例如 `0.4.0-beta.1`）会标记为 prerelease，不替换最新正式版。工作流使用内置 `GITHUB_TOKEN` 的 `contents: write` 权限，不需要额外 PAT 或模型 API 密钥。

[Release 描述生成脚本](../scripts/release.mjs) 读取当前提交之前可达、版本最高的已发布 Release 标签，正式版以上一个正式 Release 为基准，以 `上一版标签..本次提交` 获取全部提交（包含各次 push 的提交，而非仅最后一次 push）。没有上一版时读取全部历史。说明按 `feat`、`fix`、`perf`、`refactor`、`docs`、`build/ci/chore`、`test` 分类，支持英文冒号和中文全角冒号；其他提交也会完整保留，并附提交链接、完整比较链接与便携包升级说明。`BREAKING CHANGE:` 或带 `!` 的提交会加入升级注意事项。分类与描述来自提交信息，不调用外部 AI 服务。

本地可先预览说明（将上一版标签替换为实际值）：

```powershell
node scripts/release.mjs notes --repository a08381/AutoBiubiu --previous-tag 0.3.1
npm run test:release
```

## 诊断与常见问题

需要定位连接过程时，可使用可选的诊断脚本：

```powershell
powershell.exe -NoProfile -File scripts\diagnose-client-start.ps1 -Executable src-tauri\target\debug\autobiubiu.exe -Port 9222
```

该脚本启动指定 EXE、点击一次连接并检查页面状态；不会代为确认 UAC 或客户端重启，也不执行活动任务，结束后保留窗口。

| 现象 | 处理方式 |
| --- | --- |
| 浏览器预览提示桌面功能不可用 | 使用 `npm start` 或构建后的 EXE，不是 `npm run dev` |
| 没有识别到客户端 | 在设置中浏览安装目录或填写 `biubiu.exe` 完整路径，保存后重新连接 |
| 连接失败或端口占用 | 检查日志页与 `events.jsonl`，核对调试端口、UAC 和客户端页面就绪状态 |
| 任务计时不增长 | 核对实际客户端的游戏、区服和加速状态；程序仅累计已确认的加速区间 |
| 领取/抽奖处于待核对 | 先在官方页面核对结果，再重新读取，避免重复操作 |
| 关闭窗口后仍在运行 | 默认收起到托盘；可在设置中改为直接退出，或使用设置页、托盘菜单退出 |

## 项目结构

| 目录 / 文件 | 用途 |
| --- | --- |
| [src/](../src/) | Svelte 界面、IPC 封装与前端数据类型 |
| [src-tauri/src/](../src-tauri/src/) | Rust 后端、任务队列、客户端连接与本地存储 |
| [src-tauri/tauri.conf.json](../src-tauri/tauri.conf.json) | 桌面窗口与安装包配置 |
| [scripts/](../scripts/) | 构建、启动、诊断、发布与隔离验证工具 |
| [test/](../test/) | 前端、发布脚本与 PowerShell 测试；Rust 测试位于各模块内 |
| [assets/](../assets/) | 应用图标，构建时必需 |
| [docs/protocol.md](protocol.md) | biubiu 客户端连接、命令与数据解析 |
| [docs/architecture.md](architecture.md) | 组件职责、桌面命令与更新检测 |
| [docs/activity.md](activity.md) | 官方活动请求、任务执行与结果核对 |
| [docs/data.md](data.md) | 本地文件、环境变量与旧数据迁移 |

[.gitignore](../.gitignore) 排除依赖、构建产物、本地数据、历史研究样本、日志和本地凭据配置；源码、图标、测试与锁文件保持受版本控制。

## 许可证

本项目源码采用 [MIT License](../LICENSE)，Copyright (c) 2026 a08381。
