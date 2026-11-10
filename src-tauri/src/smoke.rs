use crate::Desktop;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};
use tauri::{Manager, State};

// 显式启动的隔离模式才允许测试命令，常规启动不开放测试权限。
pub struct Smoke {
    dir: Option<PathBuf>,
    phase: AtomicUsize,
    second: AtomicBool,
    checks: Mutex<Value>,
}

impl Smoke {
    pub fn from_args() -> Result<Self, String> {
        let dir =
            std::env::args().find_map(|arg| arg.strip_prefix("--smoke-test=").map(PathBuf::from));
        if let Some(dir) = &dir {
            if !dir.is_absolute() || !dir.is_dir() {
                return Err("冒烟测试目录须为已创建的绝对路径".into());
            }
            if std::fs::read_dir(dir)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
            {
                return Err("冒烟测试须使用新的空目录，以免覆盖已有记录".into());
            }
            let fixture = dir.join("client-fixture");
            std::fs::create_dir(&fixture).map_err(|e| e.to_string())?;
            // 仅用于路径存在性校验；连接命令在隔离模式中始终被阻止。
            std::fs::write(
                fixture.join("biubiu.exe"),
                b"AutoBiubiu settings test fixture; not executable",
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            dir,
            phase: AtomicUsize::new(0),
            second: AtomicBool::new(false),
            checks: Mutex::new(json!({})),
        })
    }
    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
    pub fn second_instance(&self) {
        self.second.store(true, Ordering::SeqCst);
    }
    // 合成数据只用于显式隔离模式的界面检查，普通窗口始终返回真实快照。
    pub fn decorate_snapshot(&self, mut snapshot: Value) -> Value {
        if !self.enabled() {
            return snapshot;
        }
        let names = [
            "周末游戏时光，加速领取限时好礼",
            "冬日联机计划，和好友一起出发",
            "新游体验季，开启下一段冒险",
            "每日加速打卡，积攒更多惊喜",
            "经典游戏回归，重温你的热爱",
            "本月会员福利，解锁专属礼遇",
        ];
        let activities: Vec<Value> = names.iter().enumerate().map(|(index, name)| {
            let id = format!("ui-fixture-{index}");
            let game = if index == 0 { "冒险游戏" } else { "联机游戏" };
            json!({
                "id":id, "name":name,
                "url":format!("https://www.biubiu001.com/activity/activityTemplate?activityUuid={id}"),
                "endTime":"2026-11-30T15:59:59Z", "tickets":0,
                "tasks":[
                    {"id":1,"type":7,"title":format!("加速「{game}」10 分钟"),"gameName":game,
                     "description":"在 PC 客户端保持加速，达到时长后核对任务状态。",
                     "supported":true,"status":0,"seconds":600,"awardNum":1},
                    {"id":2,"type":7,"title":"每日加速打卡","gameName":"冒险游戏",
                     "supported":true,"status":0,"seconds":300,"awardNum":1},
                    {"id":3,"type":11,"title":"邀请好友参与","description":"请在官方活动页手动完成。"}
                ]
            })
        }).collect();
        let groups: Vec<Value> = activities
            .iter()
            .enumerate()
            .map(|(index, activity)| {
                json!({
                    "id":format!("group-{index}"), "gameId":index + 1,
                    "gameName":activity["tasks"][0]["gameName"], "seconds":600,
                    "tasks":[{"key":format!("ui-fixture-{index}:1"),"activityId":activity["id"],
                        "taskId":1,"title":activity["tasks"][0]["title"],"seconds":600,"status":0}]
                })
            })
            .collect();
        snapshot["activities"] = json!(activities);
        snapshot["plan"] = json!({"groups":groups,"skipped":[],"totalSeconds":3600});
        snapshot["updatedAt"] = json!("2026-11-10T02:10:00Z");
        // 仅显式冒烟模式可启用会员展示夹具，不接入真实账号或客户端。
        if std::env::var("AUTOBIUBIU_SMOKE_MEMBERSHIP").as_deref() == Ok("1") {
            snapshot["connection"] = json!({"connected":true,"loggedIn":true});
            snapshot["membership"] = json!({
                "expiresAt":1794499199000_i64,"kind":"full","checkedAt":crate::domain::now_ms()
            });
        }
        snapshot["run"]["logs"] = json!([
            {"time":"2026-11-10T02:09:58Z","level":"info","message":"界面测试数据已就绪，未连接客户端。"},
            {"time":"2026-11-10T02:10:00Z","level":"info","message":"已读取本机活动缓存，等待开始任务。"}
        ]);
        snapshot
    }
    pub fn ready(&self) -> Result<(), std::io::Error> {
        if let Some(dir) = &self.dir {
            std::fs::write(
                dir.join("ready.json"),
                json!({"pid":std::process::id()}).to_string(),
            )?;
        }
        Ok(())
    }
}

pub fn on_loaded(webview: &tauri::Webview) {
    let Some(state) = webview.app_handle().try_state::<Desktop>() else {
        return;
    };
    if state.smoke.enabled() {
        if let Err(error) = webview.eval(include_str!("smoke.js")) {
            eprintln!("冒烟界面检查失败：{error}");
        }
    }
}

#[tauri::command]
pub async fn smoke_report(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    report: Value,
) -> Result<(), String> {
    if !state.smoke.enabled() {
        return Err("当前不是冒烟测试模式".into());
    }
    let result = inspect(&app, &state, &report).await;
    if let Err(error) = &result {
        let _ = finish(&app, &state, json!({"passed":false,"error":error}), 1).await;
    }
    result
}

async fn inspect(app: &tauri::AppHandle, state: &Desktop, report: &Value) -> Result<(), String> {
    if let Some(error) = report["error"].as_str() {
        return Err(error.to_owned());
    }
    let ui_checks = [
        "rendered",
        "ipc",
        "importValidation",
        "operationsBlocked",
        "navigation",
        "themeSwitch",
        "activityFilter",
        "activitySelection",
        "activityDetails",
        "windowControls",
        "desktopLayout",
        "membershipDisplay",
        "clientSettings",
    ];
    if ui_checks.iter().any(|key| report[*key] != true) {
        return Err("冒烟报告缺少界面检查结果".into());
    }
    let phase = state.smoke.phase.fetch_add(1, Ordering::SeqCst);
    if phase == 0 {
        let window = app.get_webview_window("main").ok_or("主窗口缺失")?;
        if app.tray_by_id("main-tray").is_none() {
            return Err("托盘图标缺失".into());
        }
        std::fs::write(
            state
                .smoke
                .dir()
                .ok_or("测试目录缺失")?
                .join("ui-ready.json"),
            "{}",
        )
        .map_err(|e| e.to_string())?;
        for _ in 0..300 {
            if state.smoke.second.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !state.smoke.second.load(Ordering::SeqCst) {
            return Err("未收到第二实例唤回事件".into());
        }
        window.close().map_err(|e| e.to_string())?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        if window.is_visible().map_err(|e| e.to_string())? {
            return Err("关闭窗口未收起到托盘".into());
        }
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        let mut checks = json!({ "tray":true, "closeToTray":true, "singleInstance":true });
        for key in ui_checks {
            checks[key] = report[key].clone();
        }
        *state.smoke.checks.lock().map_err(|e| e.to_string())? = checks;
        // 第二轮在最小窗口尺寸重复交互检查，验证分栏不会被裁切。
        window
            .set_size(tauri::LogicalSize::new(900.0, 640.0))
            .map_err(|e| e.to_string())?;
        window
            .eval("window.location.reload()")
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    if phase != 1 {
        return Err("冒烟检查重复提交".into());
    }
    let mut checks = state
        .smoke
        .checks
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    checks["reload"] = json!(true);
    checks["smallWindow"] = report["desktopLayout"].clone();
    finish(
        app,
        state,
        json!({"passed":true,"checks":checks,"version":env!("CARGO_PKG_VERSION")}),
        0,
    )
    .await
}

async fn finish(
    app: &tauri::AppHandle,
    state: &Desktop,
    mut report: Value,
    code: i32,
) -> Result<(), String> {
    state.engine.shutdown().await?;
    report["shutdown"] = json!(true);
    let dir = state.smoke.dir().ok_or("测试目录缺失")?;
    std::fs::write(
        dir.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    state.exit_allowed.store(true, Ordering::SeqCst);
    app.exit(code);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(dir: Option<PathBuf>) -> Smoke {
        Smoke {
            dir,
            phase: AtomicUsize::new(0),
            second: AtomicBool::new(false),
            checks: Mutex::new(json!({})),
        }
    }

    #[test]
    fn normal_mode_preserves_the_real_snapshot() {
        let snapshot = json!({"activities":[{"id":"real"}],"run":{"status":"running"}});
        assert_eq!(mode(None).decorate_snapshot(snapshot.clone()), snapshot);
    }

    #[test]
    fn isolated_fixture_keeps_connection_and_run_status() {
        let snapshot = json!({
            "connection":{"connected":false,"loggedIn":false},
            "run":{"status":"idle","elapsed":0,"pending":null}
        });
        let fixture = mode(Some(PathBuf::from("fixture"))).decorate_snapshot(snapshot.clone());
        assert_eq!(fixture["activities"].as_array().unwrap().len(), 6);
        assert_eq!(fixture["plan"]["groups"].as_array().unwrap().len(), 6);
        assert_eq!(fixture["connection"], snapshot["connection"]);
        assert_eq!(fixture["run"]["status"], "idle");
        assert_eq!(fixture["run"]["elapsed"], 0);
        assert_eq!(fixture["run"]["pending"], Value::Null);
    }
}
