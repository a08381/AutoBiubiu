mod backend;
mod client_platform;
mod client_settings;
mod domain;
mod engine;
mod lease;
mod membership;
mod smoke;
mod storage;

use engine::Engine;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{
    menu::{MenuBuilder, SubmenuBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

struct Desktop {
    engine: Arc<Engine>,
    exit_requested: AtomicBool,
    exit_allowed: AtomicBool,
    smoke: smoke::Smoke,
}

#[tauri::command]
async fn get_state(state: State<'_, Desktop>) -> Result<Value, String> {
    Ok(state.smoke.decorate_snapshot(state.engine.snapshot().await))
}

#[tauri::command]
async fn dispatch(
    state: State<'_, Desktop>,
    action: String,
    payload: Value,
) -> Result<Value, String> {
    // 冒烟模式仅允许在隔离目录保存客户端设置，业务操作均不接入真实客户端。
    if state.smoke.enabled() && action != "client-settings" {
        return Err("冒烟测试模式不执行客户端操作".to_owned());
    }
    state.engine.dispatch(&action, payload).await
}

#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let id = domain::activity_id(&url)?;
    app.opener()
        .open_url(domain::public_url(&id), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_data_dir(app: tauri::AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    app.opener()
        .open_path(state.engine.storage.dir().to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn detect_client_installations(
    state: State<'_, Desktop>,
) -> Result<Vec<client_platform::Installation>, String> {
    if let Some(dir) = state.smoke.dir() {
        return Ok(vec![client_platform::Installation {
            executable_path: dir
                .join("client-fixture/biubiu.exe")
                .to_string_lossy()
                .into_owned(),
            source: "隔离测试安装列表".into(),
        }]);
    }
    tokio::task::spawn_blocking(client_platform::discover_installations)
        .await
        .map_err(|e| format!("读取安装列表失败：{e}"))?
}

#[tauri::command]
async fn pick_client_directory(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
) -> Result<Option<String>, String> {
    if state.smoke.enabled() {
        return Err("冒烟测试模式不打开系统目录选择器".into());
    }
    tokio::task::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("选择 biubiu 安装目录（包含 biubiu.exe）");
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.set_parent(&window);
        }
        dialog
            .blocking_pick_folder()
            .map(|path| {
                path.into_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|e| e.to_string())
            })
            .transpose()
    })
    .await
    .map_err(|e| format!("选择安装目录失败：{e}"))?
}

#[tauri::command]
fn request_exit(app: tauri::AppHandle) {
    begin_exit(&app);
}

fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn begin_exit(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Desktop>() else {
        return;
    };
    if state.exit_requested.swap(true, Ordering::SeqCst) {
        return;
    }
    let engine = Arc::clone(&state.engine);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if engine.busy.load(Ordering::SeqCst) || engine.has_operation() {
            let confirmed = app
                .dialog()
                .message("将等待当前请求结束，停止本工具启动的加速，并保存任务与奖品记录。")
                .title("任务仍在执行，停止后退出？")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "停止并退出".into(),
                    "继续运行".into(),
                ))
                .blocking_show();
            if !confirmed {
                app.state::<Desktop>()
                    .exit_requested
                    .store(false, Ordering::SeqCst);
                return;
            }
        }
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_title("AutoBiubiu · 正在保存并退出…");
        }
        match engine.shutdown().await {
            Ok(()) => {
                app.state::<Desktop>()
                    .exit_allowed
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            }
            Err(error) => {
                // 停止或落盘失败时保留窗口，让用户核对后重试退出。
                engine.closing.store(false, Ordering::SeqCst);
                app.state::<Desktop>()
                    .exit_requested
                    .store(false, Ordering::SeqCst);
                show_window(&app);
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_title("AutoBiubiu · 请检查退出错误");
                }
                app.dialog()
                    .message(format!("{error}\n请检查客户端状态和记录目录，再重试退出。"))
                    .title("退出未完成")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
    });
}

fn menu_action(app: &tauri::AppHandle, id: &str) {
    match id {
        "show" => show_window(app),
        "hide" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        "reload" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("window.location.reload()");
            }
        }
        "data" => {
            if let Err(error) = open_data_dir(app.clone(), app.state::<Desktop>()) {
                app.dialog()
                    .message(error)
                    .title("打开记录目录失败")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
        "quit" => begin_exit(app),
        "about" => {
            app.dialog().message(format!("AutoBiubiu {}\nTauri · Rust · Svelte\n\n关闭窗口后继续在托盘运行。彻底退出请使用应用或托盘菜单。\n活动及奖品记录仅保存在本机。", env!("CARGO_PKG_VERSION")))
                .title("关于 AutoBiubiu").show(|_| {});
        }
        _ => {}
    }
}

fn install_menus(app: &tauri::App) -> tauri::Result<()> {
    let application = SubmenuBuilder::new(app, "应用")
        .text("data", "打开记录目录")
        .text("hide", "收起到托盘")
        .separator()
        .text("quit", "退出")
        .build()?;
    let view = SubmenuBuilder::new(app, "视图")
        .text("reload", "重新加载界面")
        .build()?;
    let help = SubmenuBuilder::new(app, "帮助")
        .text("about", "关于 AutoBiubiu")
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&application, &view, &help])
        .build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| menu_action(app, event.id().as_ref()));
    let tray_menu = MenuBuilder::new(app)
        .text("show", "打开 AutoBiubiu")
        .text("data", "打开记录目录")
        .separator()
        .text("quit", "退出")
        .build()?;
    TrayIconBuilder::with_id("main-tray")
        .tooltip("AutoBiubiu · 活动助手")
        .icon(
            app.default_window_icon()
                .ok_or_else(|| tauri::Error::AssetNotFound("应用图标".into()))?
                .clone(),
        )
        .menu(&tray_menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| menu_action(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn debug_port() -> Result<u16, String> {
    match std::env::var("AUTOBIUBIU_CDP_PORT") {
        Ok(value) => value
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| "AUTOBIUBIU_CDP_PORT 必须是 1–65535".to_owned()),
        Err(std::env::VarError::NotPresent) => Ok(9222),
        Err(_) => Err("AUTOBIUBIU_CDP_PORT 编码无效".to_owned()),
    }
}

fn legacy_directory() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AUTOBIUBIU_LEGACY_DATA_DIR") {
        return Some(path.into());
    }
    // 迁移来源优先于旧的 %APPDATA% 目录，其次才是 EXE 旁的历史 data/。
    if let Some(path) = appdata_legacy_directory() {
        return Some(path);
    }
    if cfg!(debug_assertions) {
        return Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data"));
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("data")))
}

/// 0.3.x 曾把记录放在 %APPDATA%\AutoBiubiu\data；老版本升级时自动搬回运行目录。
fn appdata_legacy_directory() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let dir = PathBuf::from(appdata)
        .join("AutoBiubiu")
        .join("data");
    dir.is_dir().then_some(dir)
}

fn running_data_directory() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("data")))
        .ok_or_else(|| "无法确定程序运行目录".into())
}

fn initialize(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let smoke = smoke::Smoke::from_args()?;
    let data_dir = if let Some(root) = smoke.dir() {
        root.join("profile/data")
    } else if let Some(dir) = std::env::var_os("AUTOBIUBIU_DATA_DIR") {
        PathBuf::from(dir)
    } else {
        running_data_directory()?
    };
    // 已有界面设置优先；环境变量仅作为首次配置的端口默认值。
    let port = if data_dir.join("settings.json").is_file() {
        9222
    } else {
        debug_port()?
    };
    let legacy = if smoke.enabled() {
        None
    } else {
        legacy_directory()
    };
    let engine = tauri::async_runtime::block_on(async {
        storage::prepare_data_directory(&data_dir, legacy.as_deref(), port).await?;
        let storage = storage::Storage::new(data_dir)?;
        let settings =
            client_settings::ClientSettings::load(&storage, port, !smoke.enabled()).await?;
        let port = settings.debug_port;
        let backend =
            backend::Backend::new(storage.clone(), port, settings.executable_path.into()).await?;
        Engine::new(Box::new(backend), storage, port).await
    })
    .map_err(std::io::Error::other)?;
    app.manage(Desktop {
        engine,
        exit_requested: AtomicBool::new(false),
        exit_allowed: AtomicBool::new(false),
        smoke,
    });
    install_menus(app)?;
    app.state::<Desktop>().smoke.ready()?;
    Ok(())
}

fn allowed_navigation(url: &url::Url) -> bool {
    // 默认只允许打包来源和开发服务器；业务链接一律经白名单命令交给浏览器。
    if url.scheme() == "tauri" && url.host_str() == Some("localhost") {
        return true;
    }
    if matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost") {
        return true;
    }
    cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.port() == Some(1420)
        && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
}

pub fn run() {
    // 提权辅助进程不初始化窗口、存储和单实例插件。
    if let Some(code) = client_platform::handle_helper_args() {
        std::process::exit(code);
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_window(app);
            if let Some(state) = app.try_state::<Desktop>() {
                state.smoke.second_instance();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("navigation-guard")
                .on_navigation(|_, url| allowed_navigation(url))
                .build(),
        )
        .setup(initialize)
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                smoke::on_loaded(webview);
            }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let allowed = window
                    .app_handle()
                    .try_state::<Desktop>()
                    .is_some_and(|state| state.exit_allowed.load(Ordering::SeqCst));
                if !allowed {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            dispatch,
            open_external,
            open_data_dir,
            request_exit,
            detect_client_installations,
            pick_client_directory,
            smoke::smoke_report
        ])
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let allowed = app
                    .try_state::<Desktop>()
                    .is_some_and(|state| state.exit_allowed.load(Ordering::SeqCst));
                if !allowed {
                    api.prevent_exit();
                    begin_exit(app);
                }
            }
        }),
        Err(error) => {
            eprintln!("AutoBiubiu 启动失败：{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_stays_in_app() {
        for value in [
            "https://tauri.localhost/",
            "http://tauri.localhost/",
            "tauri://localhost/",
        ] {
            assert!(allowed_navigation(&url::Url::parse(value).unwrap()));
        }
        for value in [
            "https://example.org/",
            "file:///C:/Windows/test",
            "https://tauri.localhost.evil/",
            "javascript:alert(1)",
        ] {
            assert!(!allowed_navigation(&url::Url::parse(value).unwrap()));
        }
    }
}
