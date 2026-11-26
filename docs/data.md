# 记录与配置文件

本文说明 AutoBiubiu 在本机保存哪些文件、保存在哪里，以及如何指定数据目录和迁移旧数据。

## 数据目录

默认数据目录是 **EXE 所在目录下的 `data\`**，不是固定的源码根目录。可通过 **设置 → 本机记录目录 → 打开目录** 或奖励/日志页的 **打开记录目录** 找到实际位置。

| 数据 | 持久化位置 |
| --- | --- |
| 客户端安装路径、调试端口 | `settings.json` |
| 任务完成后自动关机、关闭窗口行为 | `app-settings.json` |
| 活动缓存 | `activities.json` |
| 游戏、区服与时长映射 | `mappings.json` |
| 执行状态、奖品、待核对操作 | `run.json` |
| 过滤敏感字段后的事件与连接诊断日志 | `events.jsonl` |
| Cookie、CSRF、客户端会话、会员快照 | 仅进程内存 |

界面主题保存在 WebView 的 `localStorage`（键名 `autobiubiu-theme`）。

应用不保存账号密码；Cookie、CSRF 令牌与客户端会话只驻留后端内存，日志写入前会过滤敏感字段。数据目录、验证输出和历史样本不纳入版本控制。迁移个人数据时，应先完全退出应用，再备份整个数据目录。

## 环境变量

| 环境变量 | 用途 |
| --- | --- |
| `AUTOBIUBIU_DATA_DIR` | 指定数据目录 |
| `AUTOBIUBIU_CDP_PORT` | 尚无设置文件时的默认端口，已有 `settings.json` 优先 |
| `AUTOBIUBIU_LEGACY_DATA_DIR` | 指定首次迁移来源 |

## 旧数据迁移

首次迁移只在目标目录不存在时复制上表六个文件，不删除来源、不覆盖已有目标。来源优先级为 `AUTOBIUBIU_LEGACY_DATA_DIR`、存在的 `%APPDATA%\AutoBiubiu\data`，最后为开发版源码根目录的旧数据或发行版 EXE 旁的历史数据。[启动脚本](../scripts/launch.ps1)会显式选择源码根目录旧数据；冒烟模式完全禁用迁移。

## 升级

便携包不包含个人 `data`、配置、日志或 WebView2 运行时；数据目录首次运行时自动创建。升级时先退出旧版，只替换 EXE 并保留原 `data` 目录。
