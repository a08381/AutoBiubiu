# biubiu 客户端协议

本文说明 AutoBiubiu 如何连接本机 biubiu PC 客户端、调用哪些客户端命令，以及如何解析加速、游戏平台和会员数据。组件结构见 [架构说明](architecture.md)，活动服务与任务执行见 [活动服务与任务执行](activity.md)。调整实现时同步更新相应测试。

## 连接方式

桌面后端通过本机 `127.0.0.1:<端口>` 的 Chrome DevTools Protocol 连接 biubiu 主页面，再调用预加载脚本公开的 `window.ipc.send`。默认端口为 `9222`，合法范围为 `1–65535`。

## 客户端设置

[客户端设置](../src-tauri/src/client_settings.rs)持久化安装路径与端口。首次识别只读查询 HKCU/HKLM 的 App Paths、32/64 位卸载安装列表及常见目录。`client-settings` 操作验证 EXE 和端口，先保存，再清空旧会话与会员状态、更新任务租约端口；运行中、请求处理中、有待核对记录或自有遗留加速时不允许修改。保存设置不会启动客户端。

安装识别、Windows 原生进程启动与按需提权由[平台集成](../src-tauri/src/client_platform.rs)实现。

## 连接流程

1. 尝试已有 CDP 页面；失败后通过临时绑定检查本机端口是否可用。端口占用或系统禁止绑定时返回具体错误，不盲目启动进程。
2. 需要启动时使用 Windows API；release 通过内嵌 manifest 请求管理员权限，开发版按需通过 `ShellExecuteExW/runas` 启动自身辅助模式。
3. 若需重启已有客户端，先按完整 EXE 路径确认目标，再显示默认取消的确认窗口，只处理所选安装对应的进程。
4. 以所选端口和回环地址启动客户端，检查早期退出并有界等待页面就绪，再建立会话。

子进程环境排除 `ELECTRON_RUN_AS_NODE`、`ELECTRON_NO_ASAR`、`NODE_OPTIONS`、`NODE_PATH`，不修改系统环境。连接事件记录阶段、端口与耗时，包括 `attach`、`attach-unavailable`、`launch`、`wait-renderer`、`renderer-ready`；不记录账号令牌、Cookie 或完整用户对象。

## 客户端命令

| 命令 | 用途 |
| --- | --- |
| `get_active_center_info` | 读取活动中心条目 |
| `get-user-info` | 检查登录状态，并只提取会员到期时间与会员类型 |
| `update-user-info` | 用户点击会员刷新时，让客户端更新账号资料 |
| `search-game-list-combine` | 按任务的 `searchPlatformIds` 搜索对应平台游戏 |
| `get-games-by-ids`、`get-local-game-config` | 读取游戏、区服与加速模式 |
| `start-accelerate`、`get-accelerate-data`、`stop-accelerate` | 启动、核对与停止实际加速 |
| `open-webview` | 打开活动页面并沿用客户端会话 |

活动中心列表由 biubiu 客户端的原生网络模块读取，路径为 `/api/ping-server.active.pc.center.get?ver=1.0.0`。

## 加速状态判定

`get-accelerate-data` 的顶层 `code` 是加速引擎结果，不是 IPC 成功码：

| 返回值 | 处理 |
| --- | --- |
| 缺少 `code` | 空闲或尚未确认，归一化为 `active: false, code: null` |
| 数值 `code: 0` 且游戏编号有效 | 确认正在加速，仍需核对目标游戏、区服及预检限制 |
| 非零结果码 | 保留加速失败结果 |

空闲、启动等待和停止重置可能返回 `{ bytes: 0, code: undefined, msg: "" }`，CDP 序列化后会省略 `code`，不能把缺字段当作成功。即使没有 `code`，仍保留 `checkSpeedUpInfo` 中的限制原因。

默认节点为 `{ id: "auto", displayName: "智能节点", type: 1 }`。停止操作仅针对本应用记录为拥有的加速任务。

## 游戏平台与主机任务

本机 biubiu PC 客户端的 `resources/app.asar` 中，`dist/renderer/renderer.js` 与 `dist/main/main.js` 均包含以下 `GameType` 枚举：

| 枚举名称 | 编号 | 含义 |
| --- | --- | --- |
| `ALL` | `0` | 全部 |
| `ANDROID` | `2` | Android |
| `IOS` | `3` | iOS |
| `PC_SIMULATOR` | `4` | PC 模拟器 |
| `PC_WEB` | `5` | PC 网页游戏 |
| `WINDOWS` | `6` | Windows / PC |
| `SWITCH` | `7` | Switch |
| `PS` | `8` | PlayStation |
| `XBOX` | `9` | Xbox |
| `STEAM_DECK` | `10` | Steam Deck |
| `LIMIT_FREE` | `11` | 限免分类 |
| `INSTALL` | `12` | 安装分类 |

其中 `0`、`11`、`12` 为筛选分类，枚举未列出 `1`。

AutoBiubiu 当前的 `supported_platform` 仅接受 `6..=10`，这是本应用的支持范围。客户端普通“全部”搜索的 `searchPlatformIds` 也只包含 `6`、`7`、`8`、`9`、`10`。

任务描述中的“使用 PC 客户端”不等于目标游戏是 PC 版；明确的 PS/Xbox 等游戏端标记优先。平台不明确的主机任务不会默认为 PC。

平台随活动任务、游戏搜索结果、持久化映射、计划分组和实际加速状态一同传递。搜索只请求并接收对应平台；设置映射及启动前再次核对 `gameInfo.platformId`。启动沿用目录中的完整 `gameData` 与可用的 `speedupModelList`，不硬编码 GTA 游戏编号或主机加速模式。成功状态从 `gameData.gameInfo.platformId` 提取平台，计时同时核对游戏、平台与区服。

旧映射缺少 `platformId` 时仅按旧版 PC 映射处理。刷新活动时删除与任务平台不符的旧映射（包括手动映射）并重新搜索；尚未刷新时计划会拒绝使用这些错误映射。PS 与 Xbox 分组隔离，不能共用同一次加速的时长。

## 会员状态

[会员解析](../src-tauri/src/membership.rs)读取 `fullMemberInfo.validTime`，为空时兼容 `fullPlatformMemberValidTime`，两者按毫秒时间戳解释；`fullMemberInfo.status` 的 `1` 为正式会员、`2` 为试用会员。

客户端侧仅投影登录布尔值、到期时间和会员类型，不返回完整用户对象、手机号或登录票据。桌面快照为 `membership: { expiresAt, kind, checkedAt }`，只驻留内存，退出登录或检测到连接失败时清空。`checkedAt` 是读取客户端的时间。

显式 `membership` 操作先调用 `update-user-info` 再读取，不启动或重启客户端。界面统一使用 `Asia/Shanghai` 显示日期和秒，不从奖励天数猜算到期时间。
