use crate::domain::{self, ORIGIN, ROUTE};
use crate::storage::Storage;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{
    HeaderMap, HeaderValue, CONTENT_TYPE, COOKIE, ORIGIN as ORIGIN_HEADER, REFERER, USER_AGENT,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Instant};
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};
use url::{Host, Url};

const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const CDP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    /// True only for a definite business rejection or a request not sent at all.
    /// Transport failures, redirects and malformed responses leave the result unknown.
    pub definitive: bool,
}

impl ApiError {
    fn known(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            definitive: true,
        }
    }
    fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            definitive: false,
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for ApiError {}

pub struct Backend {
    pub activities: Vec<Value>,
    pub mappings: Value,
    pub authenticated: bool,
    pub updated_at: Value,
    pub connection: Value,
    pub membership: Value,
    storage: Storage,
    entries: Vec<Value>,
    pub client_settings: crate::client_settings::ClientSettings,
    local_http: reqwest::Client,
    public_http: reqwest::Client,
    cdp: Option<Cdp>,
    // Session credentials are deliberately neither serializable nor Debug.
    session: Option<ActivitySession>,
    owned_game_id: Option<i64>,
    unowned_game_id: Option<i64>,
}

impl Backend {
    /// Load cached data only. No socket, subprocess, login or acceleration on startup.
    pub async fn new(
        storage: Storage,
        debug_port: u16,
        executable_path: PathBuf,
    ) -> Result<Self, String> {
        if debug_port == 0 {
            return Err("客户端连接端口无效".to_owned());
        }
        let saved = storage
            .read(
                "activities.json",
                json!({"entries":[],"activities":[],"updatedAt":null}),
            )
            .await?;
        let entries = cached_array(&saved, "entries")?
            .iter()
            .map(clean_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let activities = cached_array(&saved, "activities")?.clone();
        for activity in &activities {
            let id = activity["id"].as_str().ok_or("活动缓存缺少编号")?;
            validate_id(id)?;
            if !activity["tasks"].is_array() {
                return Err("活动缓存的任务列表格式无效".to_owned());
            }
        }
        let mappings = storage.read("mappings.json", json!({})).await?;
        if !mappings.is_object() {
            return Err("游戏映射缓存格式无效".to_owned());
        }
        let run = storage.read("run.json", json!({})).await?;
        let local_http = http_client(Duration::from_secs(3))?;
        let public_http = http_client(HTTP_TIMEOUT)?;
        Ok(Self {
            activities,
            mappings,
            entries,
            authenticated: false,
            updated_at: saved["updatedAt"].clone(),
            connection: json!({"connected":false,"loggedIn":false}),
            membership: crate::membership::unavailable(),
            storage,
            client_settings: crate::client_settings::ClientSettings {
                executable_path: executable_path.to_string_lossy().into_owned(),
                debug_port,
            },
            local_http,
            public_http,
            cdp: None,
            session: None,
            owned_game_id: positive_integer(&run["ownedGameId"]),
            unowned_game_id: None,
        })
    }

    fn clear_connection(&mut self) {
        self.session = None;
        self.authenticated = false;
        self.cdp = None;
        self.connection = json!({"connected":false,"loggedIn":false});
        self.membership = crate::membership::unavailable();
    }

    pub async fn save_client_settings(&mut self, payload: &Value) -> Result<Value, String> {
        if self.owned_game_id.is_some() {
            return Err("请先停止工具遗留的加速，再修改客户端设置".into());
        }
        let settings = crate::client_settings::ClientSettings::from_input(payload)?;
        let value = json!(settings);
        self.storage.write("settings.json", &value).await?;
        self.client_settings = settings;
        self.clear_connection();
        self.unowned_game_id = None;
        Ok(value)
    }

    async fn connection_trace(&self, phase: &str, message: String) -> Result<(), String> {
        self.storage
            .log(&json!({
                "time": domain::now_ms(), "level": "info", "action": "connect", "phase": phase,
                "port": self.client_settings.debug_port, "message": message,
            }))
            .await
    }

    // 只有用户显式连接时才启动客户端；保存配置和读取快照均不会启动进程。
    pub async fn connect(&mut self) -> Result<Value, String> {
        self.clear_connection();
        let port = self.client_settings.debug_port;
        let started = Instant::now();
        self.connection_trace("attach", format!("检查本机调试端口 {port}"))
            .await?;
        if let Err(error) = self.attach_client().await {
            self.connection_trace(
                "attach-unavailable",
                format!(
                    "已有调试连接未就绪（{} 毫秒）：{error}",
                    started.elapsed().as_millis()
                ),
            )
            .await?;
            // Windows 的 ConnectEx 可能在约 2 秒后才报告 ConnectionRefused。
            // 不再用更短的 TCP 超时判断空闲端口，否则根本到不了客户端启动步骤。
            // 临时绑定仅验证所选监听地址是否可用，立即释放后交给客户端。
            let probe = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
                format!("端口 {port} 已被占用或不允许监听（{e}），未启动客户端。原连接错误：{error}。请检查客户端或更换端口。")
            })?;
            drop(probe);
            self.connection_trace(
                "launch",
                format!(
                    "端口可用，准备启动客户端：{}",
                    self.client_settings.executable_path
                ),
            )
            .await?;
            self.launch_client().await?;
            self.connection_trace("wait-renderer", "启动请求已完成，等待客户端调试页面".into())
                .await?;
            let mut last_error = String::new();
            timeout(Duration::from_secs(40), async {
                loop {
                    match self.attach_client().await {
                        Ok(()) => return,
                        Err(error) => last_error = error,
                    }
                    sleep(Duration::from_millis(500)).await;
                }
            }).await.map_err(|_| {
                format!("已发送 biubiu 启动请求，但端口 {port} 的调试页面未就绪：{last_error}。请检查客户端是否正常打开。")
            })?;
        }
        self.connection_trace(
            "renderer-ready",
            "客户端调试页面已连接，正在检查登录状态".into(),
        )
        .await?;
        let logged_in = self.logged_in().await?;
        self.connection = json!({"connected":true,"loggedIn":logged_in});
        Ok(self.connection.clone())
    }

    pub async fn scan(&mut self, public_only: bool) -> Result<Value, String> {
        let entries = if public_only {
            self.entries.clone()
        } else {
            let center = self.ipc("get_active_center_info", json!({})).await?;
            let entries = center_entries(&center)?;
            if let Some(first) = entries.first() {
                self.obtain_session(first["activeLink"].as_str().ok_or("活动链接缺失")?)
                    .await?;
            }
            entries
        };
        if entries.is_empty() {
            return Err("首次使用请连接客户端读取活动列表，也可导入官方活动链接".to_owned());
        }
        let mut next = Vec::with_capacity(entries.len());
        for entry in &entries {
            let result = if public_only {
                self.public_info(entry).await
            } else {
                self.private_info(entry).await
            };
            match result {
                Ok(activity) => next.push(activity),
                Err(error) => {
                    let id = entry_id(entry)?;
                    next.push(json!({"id":id,"name":entry["activeName"],"url":domain::public_url(&id),"tasks":[],"error":error}));
                }
            }
        }
        let updated_at = json!(domain::now_ms());
        self.save_catalog(&entries, &next, &updated_at).await?;
        self.entries = entries;
        self.activities = next;
        self.updated_at = updated_at;
        if public_only {
            self.authenticated = false;
        } else {
            self.resolve_games().await?;
        }
        Ok(Value::Array(self.activities.clone()))
    }

    pub async fn add_url(&mut self, url: &str) -> Result<(), String> {
        let id = domain::activity_id(url)?;
        if self
            .entries
            .iter()
            .any(|entry| entry_id(entry).ok().as_deref() == Some(&id))
        {
            return Ok(());
        }
        let mut entry = json!({"activeLink":domain::public_url(&id),"activeName":""});
        let activity = self.public_info(&entry).await?;
        entry["activeName"] = activity["name"].clone();
        let mut entries = self.entries.clone();
        let mut activities = self.activities.clone();
        entries.push(entry);
        activities.push(activity);
        self.save_catalog(&entries, &activities, &self.updated_at)
            .await?;
        self.entries = entries;
        self.activities = activities;
        Ok(())
    }

    pub async fn search(&mut self, keyword: &str) -> Result<Vec<Value>, String> {
        Ok(self
            .search_raw(keyword)
            .await?
            .iter()
            .map(ui_game)
            .collect())
    }

    pub async fn set_mapping(&mut self, payload: &Value) -> Result<(), String> {
        let key = payload["key"].as_str().ok_or("任务编号无效")?;
        let game_id = positive_integer(&payload["gameId"]).ok_or("游戏编号或时间无效")?;
        let seconds = payload["seconds"]
            .as_f64()
            .filter(|n| n.is_finite() && (1.0..=86400.0).contains(n))
            .ok_or("游戏编号或时间无效")?;
        let area_id = optional_area(&payload["areaId"])?;
        let (activity_index, task_index) = self.task_position(key).ok_or("任务不存在，请刷新")?;
        let task = self.activities[activity_index]["tasks"][task_index].clone();
        validate_manual_task(&task, seconds)?;
        let game = self.game(game_id).await?;
        if game["gameInfo"]["platformId"].as_i64() != Some(6) {
            return Err("请选择 PC 游戏".to_owned());
        }
        let area = if let Some(id) = area_id {
            Some(
                game["areaList"]
                    .as_array()
                    .and_then(|areas| {
                        areas
                            .iter()
                            .find(|area| positive_integer(&area["areaId"]) == Some(id))
                    })
                    .ok_or("区服编号无效")?
                    .clone(),
            )
        } else {
            None
        };
        let mut mapping =
            json!({"gameId":game_id,"gameName":game_name(&game),"seconds":seconds,"manual":true});
        if let Some(area) = area {
            mapping["areaId"] = area["areaId"].clone();
            mapping["areaName"] = area["name"].clone();
        }
        let mut mappings = self.mappings.clone();
        mappings[key] = mapping;
        let mut activities = self.activities.clone();
        activities[activity_index]["tasks"][task_index]["supported"] = json!(true);
        activities[activity_index]["tasks"][task_index]["reason"] = json!("");
        self.storage.write("mappings.json", &mappings).await?;
        self.mappings = mappings;
        self.save_catalog(&self.entries, &activities, &self.updated_at)
            .await?;
        self.activities = activities;
        Ok(())
    }

    pub async fn connect_session(&mut self, id: &str) -> Result<(), String> {
        let entry = self.entry(id)?.clone();
        self.obtain_session(entry["activeLink"].as_str().ok_or("活动链接缺失")?)
            .await
    }

    pub async fn refresh(&mut self, id: &str) -> Result<Value, String> {
        let entry = self.entry(id)?.clone();
        let activity = self.private_info(&entry).await?;
        let mut activities = self.activities.clone();
        if let Some(existing) = activities.iter_mut().find(|a| a["id"].as_str() == Some(id)) {
            *existing = activity.clone();
        } else {
            activities.push(activity.clone());
        }
        self.save_catalog(&self.entries, &activities, &self.updated_at)
            .await?;
        self.activities = activities;
        Ok(activity)
    }

    pub async fn status(&mut self) -> Result<Value, String> {
        let raw = self.ipc("get-accelerate-data", json!({})).await?;
        normalize_status(&raw)
    }

    pub async fn start_game(
        &mut self,
        game_id: i64,
        area_id: Option<i64>,
    ) -> Result<Value, String> {
        if game_id <= 0 || area_id.is_some_and(|id| id <= 0) {
            return Err("游戏或区服编号无效".to_owned());
        }
        // Engine persists intent before calling us; intent alone must not give us
        // ownership of acceleration already running before the start request.
        if self.owned_game_id != Some(game_id) {
            self.unowned_game_id = Some(game_id);
        }
        let current = self.status().await?;
        if current["active"] == true {
            if current["gameId"].as_i64() == Some(game_id) && self.owned_game_id == Some(game_id) {
                return Ok(current);
            }
            return Err(format!(
                "正在加速 {}，请先停止当前加速",
                text(&current["gameName"])
            ));
        }
        let game = self.game(game_id).await?;
        if game["gameInfo"]["platformId"].as_i64() != Some(6) {
            return Err("请选择 PC 游戏".to_owned());
        }
        let local = self
            .ipc("get-local-game-config", json!({"gameId":game_id}))
            .await?;
        let payload = start_payload(&game, &local, area_id)?;
        // Avoid claiming a different acceleration started while metadata was loading.
        let current = self.status().await?;
        if current["active"] == true {
            return Err(format!(
                "正在加速 {}，请先停止当前加速",
                text(&current["gameName"])
            ));
        }
        self.unowned_game_id = None;
        self.owned_game_id = Some(game_id);
        let result = self.ipc("start-accelerate", payload).await?;
        if result == false {
            let status = self.status().await?;
            return Err(format!(
                "客户端未允许加速（{}）",
                status
                    .get("blockType")
                    .filter(|v| !v.is_null())
                    .or_else(|| status.get("code").filter(|v| !v.is_null()))
                    .map(domain::value_id)
                    .unwrap_or_else(|| "请查看客户端提示".to_owned())
            ));
        }
        timeout(Duration::from_secs(50), async {
            for _ in 0..45 {
                sleep(Duration::from_secs(1)).await;
                let status = self.status().await?;
                if status["active"] == true && status["gameId"].as_i64() == Some(game_id) {
                    return Ok(status);
                }
                if status["code"].as_i64().is_some_and(|code| code != 0) {
                    return Err(format!(
                        "加速失败：{}",
                        if text(&status["message"]).is_empty() {
                            domain::value_id(&status["code"])
                        } else {
                            text(&status["message"]).to_owned()
                        }
                    ));
                }
            }
            Err("等待加速成功超时，请在客户端检查状态".to_owned())
        })
        .await
        .map_err(|_| "等待加速成功超时，请在客户端检查状态".to_owned())?
    }

    pub async fn stop_game(&mut self, game_id: i64) -> Result<(), String> {
        if game_id <= 0 {
            return Err("游戏编号无效".to_owned());
        }
        if self.unowned_game_id == Some(game_id) {
            return Ok(());
        }
        let run = self.storage.read("run.json", json!({})).await?;
        if self.owned_game_id != Some(game_id)
            && positive_integer(&run["ownedGameId"]) != Some(game_id)
        {
            return Ok(());
        }
        let current = self.status().await?;
        if current["active"] != true || current["gameId"].as_i64() != Some(game_id) {
            self.owned_game_id = None;
            return Ok(());
        }
        // Check the ID and stop in the same renderer promise, narrowing the race
        // with a user manually switching games between separate CDP round trips.
        let expression = format!("(async()=>{{const s=await window.ipc.send({{command:'get-accelerate-data',data:{{}}}});if(s?.code===0&&s.gameId==={game_id}){{await window.ipc.send({{command:'stop-accelerate',data:{{}}}});return true;}}return false;}})()");
        self.evaluate_client(&expression).await?;
        let result = timeout(Duration::from_secs(12), async {
            for _ in 0..20 {
                let status = self.status().await?;
                if status["active"] != true || status["gameId"].as_i64() != Some(game_id) {
                    return Ok(());
                }
                sleep(Duration::from_millis(500)).await;
            }
            Err("停止加速后状态未确认".to_owned())
        })
        .await
        .map_err(|_| "停止加速后状态未确认".to_owned())?;
        if result.is_ok() {
            self.owned_game_id = None;
        }
        result
    }

    pub async fn open_activity(&mut self, id: &str) -> Result<(), String> {
        self.entry(id)?;
        self.open_url(&domain::public_url(id)).await
    }

    pub async fn claim(&mut self, id: &str, task_id: &Value) -> Result<Value, ApiError> {
        self.entry(id).map_err(ApiError::known)?;
        if !(task_id.is_number() || task_id.as_str().is_some_and(|s| !s.is_empty())) {
            return Err(ApiError::known("任务编号无效"));
        }
        self.request(
            "/reportTaskFinish",
            json!({"activityUuid":id,"taskId":task_id}),
        )
        .await
    }

    pub async fn draw(&mut self, id: &str) -> Result<Value, ApiError> {
        self.entry(id).map_err(ApiError::known)?;
        self.request("/draw", json!({"activityUuid":id})).await
    }

    pub async fn prize(&mut self, id: &str, inventory: &Value) -> Result<Value, String> {
        self.entry(id)?;
        if !(inventory.is_number() || inventory.as_str().is_some_and(|s| !s.is_empty())) {
            return Err("奖品记录编号无效".to_owned());
        }
        self.request(
            "/getActiveAwardDetail",
            json!({"activityUuid":id,"inventoryDetailId":inventory}),
        )
        .await
        .map_err(|error| error.message)
    }

    fn entry(&self, id: &str) -> Result<&Value, String> {
        validate_id(id)?;
        self.entries
            .iter()
            .find(|entry| entry_id(entry).ok().as_deref() == Some(id))
            .ok_or_else(|| "活动不存在，请刷新".to_owned())
    }

    fn task_position(&self, key: &str) -> Option<(usize, usize)> {
        for (ai, activity) in self.activities.iter().enumerate() {
            for (ti, task) in activity["tasks"].as_array()?.iter().enumerate() {
                if format!(
                    "{}:{}",
                    domain::value_id(&activity["id"]),
                    domain::value_id(&task["id"])
                ) == key
                {
                    return Some((ai, ti));
                }
            }
        }
        None
    }

    async fn save_catalog(
        &self,
        entries: &[Value],
        activities: &[Value],
        updated_at: &Value,
    ) -> Result<(), String> {
        self.storage
            .write(
                "activities.json",
                &json!({"entries":entries,"activities":activities,"updatedAt":updated_at}),
            )
            .await
    }

    async fn public_info(&self, entry: &Value) -> Result<Value, String> {
        let url = domain::public_url(&entry_id(entry)?);
        let response = self
            .public_http
            .get(url)
            .send()
            .await
            .map_err(|_| "活动页面连接失败或超时".to_owned())?;
        if !response.status().is_success() {
            return Err(format!("活动页面 HTTP {}", response.status().as_u16()));
        }
        let bytes = bounded_body(response).await.map_err(|e| e.message)?;
        let html = std::str::from_utf8(&bytes).map_err(|_| "活动页面编码无效".to_owned())?;
        let state = domain::parse_initial_state(html)?;
        domain::simplify_activity(entry, &state["actInfo"], &state["actConfig"])
    }

    async fn private_info(&self, entry: &Value) -> Result<Value, String> {
        let data = self
            .request("", json!({"activityUuid":entry_id(entry)?}))
            .await
            .map_err(|e| e.message)?;
        let info = data
            .get("activeInfo")
            .filter(|value| value.is_object())
            .ok_or("活动详情缺少 activeInfo")?;
        let config = info["frontMaterial"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap_or(json!({}));
        domain::simplify_activity(entry, info, &config)
    }

    async fn search_raw(&mut self, keyword: &str) -> Result<Vec<Value>, String> {
        if keyword.trim().is_empty() || keyword.encode_utf16().count() > 80 {
            return Err("搜索词无效".to_owned());
        }
        let result = self.ipc("search-game-list-combine", json!({"keyword":keyword,"searchPlatformIds":[6],"page":{"page":1,"size":30},"isLimitedFree":false})).await?;
        if result["success"] != true {
            return Err(format!("游戏搜索失败：{keyword}"));
        }
        let list = result["list"].as_array().ok_or("游戏搜索列表格式无效")?;
        Ok(list
            .iter()
            .filter(|game| {
                game["gameInfo"]["platformId"].as_i64() == Some(6)
                    && positive_integer(&game["gameInfo"]["gameId"]).is_some()
            })
            .cloned()
            .collect())
    }

    async fn game(&mut self, game_id: i64) -> Result<Value, String> {
        let games = self.ipc("get-games-by-ids", json!([game_id])).await?;
        games
            .as_array()
            .and_then(|games| {
                games
                    .iter()
                    .find(|g| positive_integer(&g["gameInfo"]["gameId"]) == Some(game_id))
            })
            .cloned()
            .ok_or_else(|| format!("游戏已下架或不可用：{game_id}"))
    }

    async fn resolve_games(&mut self) -> Result<(), String> {
        let mut cache: HashMap<String, Vec<Value>> = HashMap::new();
        let mut mappings = self.mappings.clone();
        for activity in self.activities.clone() {
            for task in activity["tasks"].as_array().into_iter().flatten() {
                let key = format!(
                    "{}:{}",
                    domain::value_id(&activity["id"]),
                    domain::value_id(&task["id"])
                );
                if task["supported"] != true || mappings[&key]["manual"] == true {
                    continue;
                }
                let query = match domain::normalize_name(text(&task["gameName"])).as_str() {
                    "apex" => "Apex英雄",
                    "pubg" => "绝地求生",
                    "cod" => "使命召唤系列",
                    _ => text(&task["gameName"]),
                }
                .to_owned();
                if query.is_empty() {
                    continue;
                }
                if !cache.contains_key(&query) {
                    // Search failure or ambiguity keeps the task visible for manual mapping.
                    cache.insert(
                        query.clone(),
                        self.search_raw(&query).await.unwrap_or_default(),
                    );
                }
                let Some(game) = domain::choose_game(&query, &cache[&query]) else {
                    continue;
                };
                let traditional = text(&task["title"]).contains("繁中");
                let area = if traditional {
                    game["areaList"]
                        .as_array()
                        .and_then(|areas| areas.iter().find(|a| text(&a["name"]).contains("繁中")))
                } else {
                    None
                };
                if traditional && area.is_none() {
                    continue;
                }
                let mut mapping = json!({"gameId":game["gameInfo"]["gameId"],"gameName":game_name(&game),"manual":false});
                if let Some(area) = area {
                    mapping["areaId"] = area["areaId"].clone();
                    mapping["areaName"] = area["name"].clone();
                }
                mappings[&key] = mapping;
            }
        }
        self.storage.write("mappings.json", &mappings).await?;
        self.mappings = mappings;
        Ok(())
    }

    async fn targets(&self) -> Result<Vec<Value>, String> {
        let response = self
            .local_http
            .get(format!(
                "http://127.0.0.1:{}/json/list",
                self.client_settings.debug_port
            ))
            .send()
            .await
            .map_err(|_| "本地调试连接失败，请点击“连接客户端”".to_owned())?;
        if !response.status().is_success() {
            return Err("读取 biubiu 调试页面失败".to_owned());
        }
        let bytes = bounded_body(response).await.map_err(|e| e.message)?;
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .ok_or_else(|| "本地调试页面列表格式无效".to_owned())
    }

    async fn attach_client(&mut self) -> Result<(), String> {
        let pages = self.targets().await?;
        let target = pages
            .iter()
            .find(|page| is_renderer(page, true))
            .or_else(|| pages.iter().find(|page| is_renderer(page, false)))
            .ok_or("没有找到 biubiu 主页面，请点击“连接客户端”")?;
        let mut cdp = Cdp::connect(
            text(&target["webSocketDebuggerUrl"]),
            self.client_settings.debug_port,
        )
        .await?;
        if cdp.evaluate("!!window.ipc?.send").await? != true {
            return Err("客户端接口版本不兼容".to_owned());
        }
        self.cdp = Some(cdp);
        self.connection["connected"] = json!(true);
        Ok(())
    }

    async fn ipc(&mut self, command: &str, data: Value) -> Result<Value, String> {
        let expression = format!(
            "window.ipc.send({})",
            json!({"command":command,"data":data})
        );
        self.evaluate_client(&expression).await
    }

    async fn evaluate_client(&mut self, expression: &str) -> Result<Value, String> {
        if self.cdp.is_none() {
            self.attach_client().await?;
        }
        let result = self
            .cdp
            .as_mut()
            .ok_or("客户端尚未连接")?
            .evaluate(expression)
            .await;
        if result.is_err() {
            self.cdp = None;
            self.connection = json!({"connected":false,"loggedIn":false});
            self.membership = crate::membership::unavailable();
        }
        result
    }

    async fn logged_in(&mut self) -> Result<bool, String> {
        self.read_membership(false).await
    }

    pub async fn refresh_membership(&mut self) -> Result<Value, String> {
        // 单独刷新不启动或重启客户端；沿用用户明确建立的连接。
        if self.connection["connected"] != true {
            return Err("请先连接客户端，再刷新会员信息".to_owned());
        }
        self.read_membership(true).await?;
        Ok(self.membership.clone())
    }

    async fn read_membership(&mut self, refresh: bool) -> Result<bool, String> {
        self.membership = crate::membership::unavailable();
        let expression = if refresh {
            format!(
                "(async()=>{{await window.ipc.send({{command:'update-user-info'}});return await {};}})()",
                crate::membership::READ_USER_MEMBERSHIP
            )
        } else {
            crate::membership::READ_USER_MEMBERSHIP.to_owned()
        };
        let user = self.evaluate_client(&expression).await?;
        let logged_in = user["loggedIn"] == true;
        self.connection["loggedIn"] = json!(logged_in);
        self.membership = crate::membership::normalize(&user, domain::now_ms());
        if !logged_in {
            self.session = None;
            self.authenticated = false;
        }
        Ok(logged_in)
    }

    async fn open_url(&mut self, url: &str) -> Result<(), String> {
        let url = domain::public_url(&domain::activity_id(url)?);
        self.ipc(
            "open-webview",
            json!({"url":url,"title":"AutoBiubiu 活动","width":1200,"height":700}),
        )
        .await?;
        Ok(())
    }

    async fn obtain_session(&mut self, first_url: &str) -> Result<(), String> {
        let id = domain::activity_id(first_url)?;
        self.session = None;
        self.authenticated = false;
        if !self.logged_in().await? {
            self.connection["loggedIn"] = json!(false);
            return Err("请先在 biubiu 客户端登录".to_owned());
        }
        let mut page = official_page(&self.targets().await?, &id);
        if page.is_none() {
            self.open_url(&domain::public_url(&id)).await?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while page.is_none() && Instant::now() < deadline {
                sleep(Duration::from_millis(500)).await;
                page = official_page(&self.targets().await?, &id);
            }
        }
        let page = page.ok_or("官方活动页未打开")?;
        let mut cdp = Cdp::connect(
            text(&page["webSocketDebuggerUrl"]),
            self.client_settings.debug_port,
        )
        .await?;
        let mut ready = false;
        timeout(Duration::from_secs(20), async {
            for _ in 0..30 {
                // Recheck the live origin; a target may navigate after /json/list.
                ready = cdp.evaluate(&format!("location.origin==={}&&location.pathname==={}&&!!window.__INITIAL_STATE__?.userInfo", json!(ORIGIN), json!(ROUTE))).await? == true;
                if ready { break; }
                sleep(Duration::from_millis(500)).await;
            }
            Ok::<(), String>(())
        }).await.map_err(|_| "活动页登录检查超时".to_owned())??;
        if !ready {
            return Err("活动页尚未登录，请在客户端重新打开活动".to_owned());
        }
        let user_agent = cdp.evaluate("navigator.userAgent").await?;
        let cookies = cdp
            .send(
                "Network.getCookies",
                json!({"urls":[format!("{ORIGIN}{ROUTE}")]}),
            )
            .await?;
        self.session = Some(ActivitySession::new(
            text(&user_agent),
            &cookies["cookies"],
        )?);
        self.authenticated = true;
        self.connection = json!({"connected":true,"loggedIn":true});
        Ok(())
    }

    async fn request(&self, action: &str, mut data: Value) -> Result<Value, ApiError> {
        if !matches!(
            action,
            "" | "/reportTaskFinish"
                | "/draw"
                | "/getActiveAwardDetail"
                | "/myprizes"
                | "/claimFreeGift"
        ) {
            return Err(ApiError::known("未知活动操作"));
        }
        let id = data["activityUuid"]
            .as_str()
            .ok_or_else(|| ApiError::known("活动编号无效"))?
            .to_owned();
        validate_id(&id).map_err(ApiError::known)?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| ApiError::known("活动登录态尚未连接"))?;
        let url = Url::parse(&format!("{ORIGIN}{ROUTE}{action}"))
            .map_err(|_| ApiError::known("活动地址无效"))?;
        let headers = session.headers(&url, &id)?;
        data["actId"] = Value::Null;
        // No application-level retries: the queue must reconcile any unknown result.
        let response = self
            .public_http
            .post(url)
            .headers(headers)
            .json(&data)
            .send()
            .await
            .map_err(|_| ApiError::unknown("活动接口连接失败或超时，操作结果待核对"))?;
        let status = response.status();
        let bytes = bounded_body(response).await?;
        // 与旧实现一致，非成功 HTTP 响应不作为确定的业务拒绝；代理错误页也可能带 code。
        if !status.is_success() {
            return Err(ApiError::unknown(format!(
                "活动接口 HTTP {}，操作结果待核对",
                status.as_u16()
            )));
        }
        let parsed = serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| ApiError::unknown("活动接口响应不是有效 JSON，操作结果待核对"))?;
        api_data(&parsed)
    }

    async fn launch_client(&self) -> Result<(), String> {
        let executable = PathBuf::from(&self.client_settings.executable_path);
        let port = self.client_settings.debug_port;
        tokio::task::spawn_blocking(move || {
            crate::client_platform::launch_client(&executable, port)
        })
        .await
        .map_err(|e| format!("启动客户端任务失败：{e}"))?
    }
}

struct Cdp {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    async fn connect(address: &str, port: u16) -> Result<Self, String> {
        let address = local_websocket_url(address, port)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_BODY))
            .max_frame_size(Some(MAX_BODY));
        let (socket, _) = timeout(
            Duration::from_secs(5),
            connect_async_with_config(address.as_str(), Some(config), false),
        )
        .await
        .map_err(|_| "本地调试连接超时".to_owned())?
        .map_err(|_| "本地调试连接失败".to_owned())?;
        Ok(Self { socket, next_id: 0 })
    }

    async fn send(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        timeout(CDP_TIMEOUT, async {
            self.socket
                .send(Message::Text(
                    json!({"id":id,"method":method,"params":params})
                        .to_string()
                        .into(),
                ))
                .await
                .map_err(|_| "biubiu 连接已断开".to_owned())?;
            while let Some(frame) = self.socket.next().await {
                let frame = frame.map_err(|_| "biubiu 调试连接读取失败".to_owned())?;
                let bytes = match frame {
                    Message::Text(text) => text.as_bytes().to_vec(),
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Ping(bytes) => {
                        self.socket
                            .send(Message::Pong(bytes))
                            .await
                            .map_err(|_| "biubiu 连接已断开".to_owned())?;
                        continue;
                    }
                    Message::Close(_) => return Err("biubiu 连接已断开".to_owned()),
                    _ => continue,
                };
                let message: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| "本地调试响应格式无效".to_owned())?;
                if message["id"].as_u64() != Some(id) {
                    continue;
                }
                if message.get("error").is_some() {
                    return Err(format!("客户端调试命令 {method} 失败"));
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "本地调试响应缺少结果".to_owned());
            }
            Err("biubiu 连接已断开".to_owned())
        })
        .await
        .map_err(|_| format!("{method} 超时，操作结果待核对"))?
    }

    async fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let result = self
            .send(
                "Runtime.evaluate",
                json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("客户端接口执行失败，请检查客户端状态".to_owned());
        }
        // JavaScript undefined is intentionally represented as JSON null.
        let remote = result
            .get("result")
            .filter(|v| v.is_object())
            .ok_or("客户端接口结果格式无效")?;
        if remote.get("value").is_none() && remote["type"] != "undefined" {
            return Err("客户端接口未返回可读取的数据".to_owned());
        }
        Ok(remote["value"].clone())
    }
}

fn http_client(duration: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5).min(duration))
        .timeout(duration)
        .http1_only()
        .build()
        .map_err(|_| "建立网络客户端失败".to_owned())
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(ApiError::unknown("接口响应过大，操作结果待核对"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiError::unknown("接口响应读取失败或超时，操作结果待核对"))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(ApiError::unknown("接口响应过大，操作结果待核对"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn local_websocket_url(address: &str, port: u16) -> Result<Url, String> {
    let mut url = Url::parse(address).map_err(|_| "本地调试地址无效".to_owned())?;
    let local = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(host)) => host == "localhost",
        None => false,
    };
    if !local
        || url.scheme() != "ws"
        || url.port_or_known_default() != Some(port)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || !url.path().starts_with("/devtools/page/")
    {
        return Err("调试地址必须指向指定端口的本机页面".to_owned());
    }
    // Never resolve a name returned by a debug endpoint through DNS.
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_| "本地调试地址无效".to_owned())?;
    }
    Ok(url)
}

fn is_renderer(page: &Value, exact: bool) -> bool {
    let url = text(&page["url"]);
    if page["type"].as_str().is_some_and(|kind| kind != "page") {
        return false;
    }
    if exact {
        url.ends_with("/renderer/index.html")
    } else {
        url.contains("/renderer/index.html") && !url.contains("background")
    }
}

fn official_page(pages: &[Value], id: &str) -> Option<Value> {
    pages
        .iter()
        .find(|p| domain::activity_id(text(&p["url"])).ok().as_deref() == Some(id))
        .or_else(|| {
            pages
                .iter()
                .find(|p| domain::activity_id(text(&p["url"])).is_ok())
        })
        .cloned()
}

struct ActivitySession {
    user_agent: HeaderValue,
    cookies: Vec<SessionCookie>,
}
struct SessionCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    expires: Option<f64>,
    secure: bool,
}

impl ActivitySession {
    fn new(user_agent: &str, raw: &Value) -> Result<Self, String> {
        let user_agent = HeaderValue::from_str(user_agent)
            .map_err(|_| "客户端 User-Agent 格式无效".to_owned())?;
        let mut cookies = Vec::new();
        for cookie in raw.as_array().ok_or("客户端 Cookie 列表格式无效")? {
            let name = text(&cookie["name"]);
            let value = text(&cookie["value"]);
            let domain = text(&cookie["domain"]).to_ascii_lowercase();
            if !matches!(
                domain.trim_start_matches('.'),
                "www.biubiu001.com" | "biubiu001.com"
            ) {
                continue;
            }
            // CDP 返回的是 Chromium 已保存的原始值，不局限于 RFC 的 cookie-octet。
            // 保留逗号、空格、引号、反斜杠和 UTF-8；转义会改变登录态或签名。
            // 仍拦截控制字符与 Cookie 分隔符，避免改变请求头或注入额外 Cookie。
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                || value.bytes().any(|b| b.is_ascii_control() || b == b';')
            {
                return Err("客户端 Cookie 格式无效".to_owned());
            }
            cookies.push(SessionCookie {
                name: name.to_owned(),
                value: value.to_owned(),
                domain,
                path: cookie["path"].as_str().unwrap_or("/").to_owned(),
                expires: cookie["expires"].as_f64(),
                secure: cookie["secure"].as_bool().unwrap_or(false),
            });
        }
        if cookies.is_empty() {
            return Err("活动登录态尚未连接".to_owned());
        }
        Ok(Self {
            user_agent,
            cookies,
        })
    }

    fn headers(&self, url: &Url, id: &str) -> Result<HeaderMap, ApiError> {
        if url.origin().ascii_serialization() != ORIGIN || !url.path().starts_with(ROUTE) {
            return Err(ApiError::known("会话请求地址无效"));
        }
        let mut cookies: Vec<_> = self
            .cookies
            .iter()
            .filter(|cookie| cookie.matches(url, domain::now_ms() as f64 / 1000.0))
            .collect();
        cookies.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        if cookies.is_empty() {
            return Err(ApiError::known("活动登录态已过期，请重新连接"));
        }
        let csrf = cookies
            .iter()
            .find(|cookie| cookie.name == "csrfToken")
            .map(|c| percent_decode(&c.value))
            .transpose()
            .map_err(ApiError::known)?
            .unwrap_or_default();
        let cookie = cookies
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        headers.insert(USER_AGENT, self.user_agent.clone());
        headers.insert(ORIGIN_HEADER, HeaderValue::from_static(ORIGIN));
        headers.insert(
            REFERER,
            HeaderValue::from_str(&domain::public_url(id))
                .map_err(|_| ApiError::known("活动地址无效"))?,
        );
        let mut cookie =
            HeaderValue::from_str(&cookie).map_err(|_| ApiError::known("活动 Cookie 格式无效"))?;
        cookie.set_sensitive(true);
        let mut csrf =
            HeaderValue::from_str(&csrf).map_err(|_| ApiError::known("活动 CSRF 字段格式无效"))?;
        csrf.set_sensitive(true);
        headers.insert(COOKIE, cookie);
        headers.insert("x-csrf-token", csrf);
        headers.insert("x-app-code", HeaderValue::from_static(""));
        Ok(headers)
    }
}

impl SessionCookie {
    fn matches(&self, url: &Url, now: f64) -> bool {
        if self.secure && url.scheme() != "https" {
            return false;
        }
        if self.expires.is_some_and(|time| time > 0.0 && time <= now) {
            return false;
        }
        let host = url.host_str().unwrap_or("");
        let domain = self.domain.trim_start_matches('.');
        let domain_matches = host == domain
            || (self.domain.starts_with('.') && host.ends_with(&format!(".{domain}")));
        let path_matches = url.path() == self.path
            || (url.path().starts_with(&self.path)
                && (self.path.ends_with('/')
                    || url.path().as_bytes().get(self.path.len()) == Some(&b'/')));
        domain_matches && path_matches
    }
}

fn percent_decode(value: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(value.len());
    let source = value.as_bytes();
    let mut index = 0;
    while index < source.len() {
        if source[index] == b'%' {
            let pair = source
                .get(index + 1..index + 3)
                .ok_or("活动 CSRF 编码无效")?;
            let high = char::from(pair[0])
                .to_digit(16)
                .ok_or("活动 CSRF 编码无效")?;
            let low = char::from(pair[1])
                .to_digit(16)
                .ok_or("活动 CSRF 编码无效")?;
            bytes.push((high * 16 + low) as u8);
            index += 3;
        } else {
            bytes.push(source[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| "活动 CSRF 编码无效".to_owned())
}

fn api_data(result: &Value) -> Result<Value, ApiError> {
    if !result.is_object() {
        return Err(ApiError::unknown("活动接口响应格式无效，操作结果待核对"));
    }
    let state = result.get("state").filter(|v| !v.is_null()).or_else(|| {
        result
            .get("data")
            .and_then(|d| d.get("state"))
            .filter(|v| !v.is_null())
    });
    let mut success_state = false;
    if let Some(state) = state {
        let code = state
            .get("code")
            .filter(|v| v.is_number() || v.is_string())
            .ok_or_else(|| ApiError::unknown("活动接口状态格式无效，操作结果待核对"))?;
        let code = domain::value_id(code);
        if code != "200" && code != "2000000" {
            let message = state["msg"]
                .as_str()
                .or_else(|| state["message"].as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("活动操作失败");
            return Err(ApiError::known(format!("{message}（{code}）")));
        }
        success_state = true;
    }
    if result["success"] == false || result["data"]["success"] == false {
        // 只有明确业务状态码才允许队列清除 pending，裸 success=false 不足以核对结果。
        return Err(ApiError::unknown("活动接口报告操作失败，操作结果待核对"));
    }
    if let Some(data) = result.get("data") {
        if data.is_object() || data.is_array() || success_state || result["success"] == true {
            return Ok(data.clone());
        }
    }
    if success_state || result["success"] == true {
        return Ok(Value::Null);
    }
    Err(ApiError::unknown(
        "活动接口响应缺少业务结果，操作结果待核对",
    ))
}

fn clean_entry(entry: &Value) -> Result<Value, String> {
    let id = entry_id(entry)?;
    // Do not cache session-bearing query parameters or arbitrary account metadata.
    let mut clean =
        json!({"activeLink":domain::public_url(&id),"activeName":text(&entry["activeName"])});
    for key in ["activeCoverImg", "startTime", "endTime"] {
        if let Some(value) = entry.get(key) {
            clean[key] = value.clone();
        }
    }
    Ok(clean)
}

fn center_entries(result: &Value) -> Result<Vec<Value>, String> {
    if result["success"] != true || !result["data"].is_object() {
        return Err("活动列表读取失败，请检查登录和网络".to_owned());
    }
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for key in ["drawActiveList", "residentActiveList"] {
        if let Some(list) = result["data"].get(key).filter(|v| !v.is_null()) {
            for entry in list.as_array().ok_or("活动入口列表格式无效")? {
                // Other official campaign types are not template activities.
                if let Ok(entry) = clean_entry(entry) {
                    let id = entry_id(&entry)?;
                    if seen.insert(id) {
                        entries.push(entry);
                    }
                }
            }
        }
    }
    Ok(entries)
}

fn cached_array<'a>(saved: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    saved
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("活动缓存字段 {key} 格式无效"))
}
fn entry_id(entry: &Value) -> Result<String, String> {
    domain::activity_id(entry["activeLink"].as_str().ok_or("活动链接缺失")?)
}
fn validate_id(id: &str) -> Result<(), String> {
    domain::activity_id(&domain::public_url(id)).map(|_| ())
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn positive_integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|v| v.is_finite() && v.fract() == 0.0 && *v < i64::MAX as f64)
                .map(|v| v as i64)
        })
        .filter(|v| *v > 0)
}
fn optional_area(value: &Value) -> Result<Option<i64>, String> {
    if value.is_null() || value.as_i64() == Some(0) {
        Ok(None)
    } else {
        positive_integer(value)
            .map(Some)
            .ok_or_else(|| "区服编号无效".to_owned())
    }
}
fn game_name(game: &Value) -> &str {
    let alias = text(&game["gameInfo"]["aliasName"]);
    if alias.is_empty() {
        text(&game["gameInfo"]["gameName"])
    } else {
        alias
    }
}
fn ui_game(game: &Value) -> Value {
    json!({"id":game["gameInfo"]["gameId"],"name":game_name(game),"english":game["gameInfo"]["gameName"],"areas":game["areaList"].as_array().cloned().unwrap_or_default()})
}
fn normalize_status(raw: &Value) -> Result<Value, String> {
    let code = raw["code"].as_i64();
    // code 是引擎结果，不是 IPC 成功码。空闲、启动中和停止后的基础对象
    // 为 {bytes:0, code:undefined, msg:""}；CDP 会省略 undefined 字段。
    // 缺失结果必须保持 null，不能补成 0，否则会把未确认状态计为加速成功。
    let awaiting_result =
        raw["code"].is_null() && raw["bytes"].is_number() && raw["msg"].is_string();
    if !raw.is_object() || (code.is_none() && !awaiting_result) {
        return Err("客户端加速状态格式无效，请检查客户端".to_owned());
    }
    let game_id = positive_integer(&raw["gameId"]);
    Ok(
        json!({"active":code == Some(0) && game_id.is_some(),"gameId":game_id,
        "gameName":game_name(&raw["gameData"]),"areaId":positive_integer(&raw["area"]["areaId"]),
        "code":code,"message":text(&raw["msg"]),"allowed":raw["checkSpeedUpInfo"]["canSpeedup"],"blockType":raw["checkSpeedUpInfo"]["blockType"]}),
    )
}
fn validate_manual_task(task: &Value, seconds: f64) -> Result<(), String> {
    // Use domain rules for mobile exclusion even on a manually overridden task.
    let activity = json!({"id":"check","tasks":[task]});
    let key = format!("check:{}", domain::value_id(&task["id"]));
    let mappings = json!({key:{"gameId":1,"seconds":seconds,"manual":true}});
    let plan = domain::build_plan(&[activity], &mappings, None, domain::now_ms());
    if task["type"].as_i64() != Some(7)
        || plan["groups"]
            .as_array()
            .is_none_or(|groups| groups.is_empty())
    {
        return Err("这不是可设置的 PC 加速任务".to_owned());
    }
    Ok(())
}
fn start_payload(
    game: &Value,
    local: &Value,
    requested_area: Option<i64>,
) -> Result<Value, String> {
    let areas = game["areaList"].as_array().ok_or("该游戏没有可用区服")?;
    let area = if let Some(id) = requested_area {
        areas
            .iter()
            .find(|area| positive_integer(&area["areaId"]) == Some(id))
            .ok_or("区服编号无效")?
    } else {
        let preferred =
            positive_integer(&local["area"]).or_else(|| text(&local["area"]).parse::<i64>().ok());
        areas
            .iter()
            .find(|area| positive_integer(&area["areaId"]) == preferred && preferred.is_some())
            .or_else(|| areas.first())
            .ok_or("该游戏没有可用区服")?
    };
    let modes = game["speedupModelList"].as_array();
    let mode = modes.and_then(|modes| {
        modes
            .iter()
            .find(|mode| mode["speedupModelId"] == local["accMode"])
            .or_else(|| modes.first())
    });
    let mut dto = json!({"area":area,"accPod":{"id":"auto","displayName":"智能节点","type":1},"rememberPod":false});
    if let Some(mode) = mode {
        dto["accMode"] = mode.clone();
    }
    Ok(json!({"gameData":game,"gameAreaDto":dto,"useGiftVip":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn status_accepts_snapshots_without_an_engine_result() {
        // 客户端初始/重置对象中的 code 为 undefined，经 CDP 序列化后会被省略。
        for raw in [
            json!({"bytes":0,"msg":""}),
            json!({"bytes":0,"code":null,"msg":"","networkDetectData":null}),
            json!({"bytes":0,"msg":"","checkSpeedUpInfo":{"canSpeedup":true}}),
            json!({"bytes":0,"msg":"","checkSpeedUpInfo":{"canSpeedup":false,"blockType":3}}),
        ] {
            let status = normalize_status(&raw).unwrap();
            assert_eq!(status["active"], false);
            assert_eq!(status["code"], Value::Null);
            assert_eq!(status["gameId"], Value::Null);
            assert_eq!(status["allowed"], raw["checkSpeedUpInfo"]["canSpeedup"]);
            assert_eq!(status["blockType"], raw["checkSpeedUpInfo"]["blockType"]);
        }
        let unconfirmed = normalize_status(&json!({
            "bytes":0,"msg":"","gameId":42
        }))
        .unwrap();
        assert_eq!(unconfirmed["active"], false);
        assert_eq!(unconfirmed["code"], Value::Null);
    }

    #[test]
    fn status_preserves_active_game_and_failure_details() {
        let active = normalize_status(&json!({
            "code":0,"gameId":42,
            "gameData":{"gameInfo":{"aliasName":"测试游戏"}},
            "area":{"areaId":7},
            "checkSpeedUpInfo":{"canSpeedup":true}
        }))
        .unwrap();
        assert_eq!(active["active"], true);
        assert_eq!(active["gameId"], 42);
        assert_eq!(active["gameName"], "测试游戏");
        assert_eq!(active["areaId"], 7);
        assert_eq!(active["allowed"], true);

        let failed = normalize_status(&json!({
            "code":-1,"gameId":42,"msg":"测试失败信息",
            "checkSpeedUpInfo":{"canSpeedup":false,"blockType":3}
        }))
        .unwrap();
        assert_eq!(failed["active"], false);
        assert_eq!(failed["code"], -1);
        assert_eq!(failed["message"], "测试失败信息");
        assert_eq!(failed["allowed"], false);
        assert_eq!(failed["blockType"], 3);
    }

    #[test]
    fn status_does_not_count_unidentified_games_or_accept_malformed_responses() {
        for game_id in [Value::Null, json!(0), json!(-1), json!(1.5), json!("42")] {
            let status = normalize_status(&json!({"code":0,"gameId":game_id})).unwrap();
            assert_eq!(status["active"], false);
            assert_eq!(status["gameId"], Value::Null);
        }
        for raw in [
            json!([]),
            json!(false),
            json!("invalid"),
            json!(0),
            json!({"code":"invalid"}),
            json!({"code":false}),
            json!({"code":0.5}),
            json!({"unexpected":true}),
        ] {
            assert!(normalize_status(&raw).is_err());
        }
    }

    #[test]
    fn cdp_accepts_only_loopback_and_expected_port() {
        assert_eq!(
            local_websocket_url("ws://localhost:9222/devtools/page/abc", 9222)
                .unwrap()
                .host_str(),
            Some("127.0.0.1")
        );
        assert!(local_websocket_url("ws://[::1]:9222/devtools/page/abc", 9222).is_ok());
        for address in [
            "ws://example.org:9222/devtools/page/a",
            "ws://127.0.0.1:9223/devtools/page/a",
            "wss://127.0.0.1:9222/devtools/page/a",
            "ws://user@127.0.0.1:9222/devtools/page/a",
            "ws://127.0.0.1:9222/other",
            "ws://127.0.0.1:9222/devtools/page/a?url=remote",
        ] {
            assert!(local_websocket_url(address, 9222).is_err(), "{address}");
        }
    }

    #[test]
    fn business_rejections_are_definitive_but_unknown_json_is_not() {
        for result in [
            json!({"state":{"code":400,"msg":"未完成"}}),
            json!({"data":{"state":{"code":"5000001","message":"活动结束"}}}),
        ] {
            assert!(api_data(&result).unwrap_err().definitive);
        }
        for result in [
            Value::Null,
            json!({}),
            json!({"state":{}}),
            json!({"data":null}),
            json!({"success":false}),
            json!({"data":{"success":false}}),
        ] {
            assert!(!api_data(&result).unwrap_err().definitive);
        }
        for code in [json!(200), json!(2000000), json!("200"), json!("2000000")] {
            assert_eq!(
                api_data(&json!({"state":{"code":code},"data":{"awardNum":1}})).unwrap(),
                json!({"awardNum":1})
            );
        }
        assert_eq!(
            api_data(&json!({"data":{"activeInfo":{}}})).unwrap(),
            json!({"activeInfo":{}})
        );
    }

    #[test]
    fn session_limits_domain_path_expiry_and_keeps_plus_in_csrf() {
        let session = ActivitySession::new("fixture", &json!([
            {"name":"csrfToken","value":"abc%2Bdef+ghi","domain":".biubiu001.com","path":"/","expires":-1,"secure":true},
            {"name":"session","value":"good","domain":"www.biubiu001.com","path":ROUTE,"expires":-1},
            {"name":"expired","value":"bad","domain":"www.biubiu001.com","path":"/","expires":1},
            {"name":"differentPath","value":"bad","domain":"www.biubiu001.com","path":"/other"},
            {"name":"foreign","value":"bad","domain":"evil.example","path":"/"}
        ])).unwrap();
        let headers = session
            .headers(
                &Url::parse(&format!("{ORIGIN}{ROUTE}/draw")).unwrap(),
                "abc",
            )
            .unwrap();
        assert_eq!(headers["x-csrf-token"], "abc+def+ghi");
        assert_eq!(headers[COOKIE], "session=good; csrfToken=abc%2Bdef+ghi");
        assert!(headers[COOKIE].is_sensitive());
        assert!(session
            .headers(
                &Url::parse("https://example.org/activity/activityTemplate/draw").unwrap(),
                "abc"
            )
            .is_err());
        assert!(percent_decode("bad%XX").is_err());
    }

    #[test]
    fn session_preserves_browser_cookie_values() {
        // Chromium 的 Cookie 值可以包含逗号、空格、引号和反斜杠，不能擅自转义。
        let values = [
            "1700000000,1700000060",
            r#"{"source":"pc", "label":"a\\b"}"#,
            r#""quoted value""#,
            "中文值",
            "",
        ];
        let url = Url::parse(&format!("{ORIGIN}{ROUTE}")).unwrap();
        for value in values {
            let session = ActivitySession::new("fixture", &json!([
                {"name":"csrfToken","value":"test-token","domain":"www.biubiu001.com","path":"/"},
                {"name":"browserValue","value":value,"domain":".biubiu001.com","path":"/"}
            ]))
            .unwrap_or_else(|_| panic!("浏览器兼容的 Cookie 被误判"));
            let headers = session.headers(&url, "abc").unwrap();
            assert_eq!(
                headers[COOKIE].as_bytes(),
                format!("csrfToken=test-token; browserValue={value}").as_bytes()
            );
            assert_eq!(headers["x-csrf-token"], "test-token");
            assert!(headers[COOKIE].is_sensitive());
            assert!(headers["x-csrf-token"].is_sensitive());
        }
    }

    #[test]
    fn session_rejects_cookie_delimiters_and_control_characters() {
        for value in [
            "value; injected=1",
            "value\r\nX-Injected: 1",
            "a\tb",
            "a\0b",
            "a\u{7f}b",
        ] {
            assert!(ActivitySession::new(
                "fixture",
                &json!([
                    {"name":"session","value":value,"domain":"www.biubiu001.com","path":"/"}
                ])
            )
            .is_err());
        }
        for name in ["", "bad=name", "bad;name", "bad\r\nname"] {
            assert!(ActivitySession::new(
                "fixture",
                &json!([
                    {"name":name,"value":"value","domain":"www.biubiu001.com","path":"/"}
                ])
            )
            .is_err());
        }
    }

    #[test]
    fn catalog_deduplicates_and_removes_session_query_parameters() {
        let result = json!({"success":true,"data":{"drawActiveList":[
            {"activeLink":format!("{}&serviceTicket=secret",domain::public_url("a")),"activeName":"A","cookie":"secret"},
            {"activeLink":"https://evil.example/"}],"residentActiveList":[{"activeLink":domain::public_url("a")}]}});
        let entries = center_entries(&result).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0],
            json!({"activeLink":domain::public_url("a"),"activeName":"A"})
        );
    }

    #[test]
    fn game_payload_preserves_pc_protocol_and_explicit_area() {
        let game = json!({"gameInfo":{"gameId":1,"platformId":6,"aliasName":"Game","gameName":"English"},
            "areaList":[{"areaId":10,"name":"一区"},{"areaId":20,"name":"二区"}],"speedupModelList":[{"speedupModelId":3},{"speedupModelId":4}]});
        let payload = start_payload(&game, &json!({"area":"20","accMode":4}), None).unwrap();
        assert_eq!(payload["gameAreaDto"]["area"]["areaId"], 20);
        assert_eq!(payload["gameAreaDto"]["accMode"]["speedupModelId"], 4);
        assert_eq!(payload["gameAreaDto"]["accPod"]["id"], "auto");
        assert_eq!(payload["useGiftVip"], false);
        assert!(start_payload(&game, &json!({}), Some(99)).is_err());
        assert_eq!(ui_game(&game)["english"], "English");
    }

    #[test]
    fn manual_mapping_uses_domain_mobile_exclusion() {
        assert!(validate_manual_task(&json!({"id":1,"type":7,"title":"自定任务"}), 60.0).is_ok());
        for field in ["title", "description"] {
            let mut task = json!({"id":1,"type":7});
            task[field] = json!("iOS端加速");
            assert!(validate_manual_task(&task, 60.0).is_err());
        }
    }

    #[tokio::test]
    async fn settings_are_saved_before_connection_and_membership_are_cleared() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let exe = root.path().join("biubiu.exe");
        std::fs::write(&exe, b"fixture").unwrap();
        let mut backend = Backend::new(storage.clone(), 9222, exe.clone())
            .await
            .unwrap();
        backend.connection = json!({"connected":true,"loggedIn":true});
        backend.authenticated = true;
        backend.membership = json!({"expiresAt":1794499199000_i64});
        let old = backend.client_settings.clone();
        assert!(backend
            .save_client_settings(&json!({"executablePath":exe,"debugPort":0}))
            .await
            .is_err());
        assert_eq!(backend.client_settings, old);
        assert_eq!(backend.connection["connected"], true);
        std::fs::write(root.path().join("settings.json"), b"broken json").unwrap();
        let payload = json!({"executablePath":exe,"debugPort":9333});
        assert!(backend.save_client_settings(&payload).await.is_err());
        assert_eq!(backend.client_settings, old);
        assert!(backend.authenticated);
        // 只修复临时夹具，验证正常落盘后才切换内存设置。
        std::fs::write(
            root.path().join("settings.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        backend.save_client_settings(&payload).await.unwrap();
        assert_eq!(backend.client_settings.debug_port, 9333);
        assert_eq!(backend.connection["connected"], false);
        assert!(!backend.authenticated);
        assert_eq!(backend.membership, crate::membership::unavailable());
        assert_eq!(
            crate::client_settings::ClientSettings::load(&storage, 9222, false)
                .await
                .unwrap(),
            backend.client_settings
        );
    }

    #[tokio::test]
    async fn closed_port_reaches_launcher_validation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let root = tempfile::tempdir().unwrap();
        let mut backend = Backend::new(
            Storage::new(root.path().to_owned()).unwrap(),
            port,
            root.path().join("biubiu.exe"),
        )
        .await
        .unwrap();
        let started = Instant::now();
        let error = backend.connect().await.unwrap_err();
        eprintln!("closed-port connect: {:?}: {error}", started.elapsed());
        assert!(
            error.contains("请选择实际存在"),
            "连接流程在启动前被截断：{error}"
        );
    }

    #[tokio::test]
    async fn occupied_port_without_a_client_page_never_launches_an_executable() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = std::sync::Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn({
            let listener = listener.clone();
            async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                socket.read(&mut request).await.unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
                    )
                    .await
                    .unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let mut backend = Backend::new(
            Storage::new(root.path().to_owned()).unwrap(),
            port,
            root.path().join("missing.exe"),
        )
        .await
        .unwrap();
        let error = backend.connect().await.unwrap_err();
        assert!(error.contains("已被占用"), "{error}");
        assert_eq!(backend.connection["connected"], false);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn new_loads_cache_without_opening_debug_socket_or_saving_credentials() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        storage.write("activities.json", &json!({"entries":[{"activeLink":domain::public_url("a")}],"activities":[{"id":"a","tasks":[]}],"updatedAt":123})).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend = Backend::new(
            storage,
            listener.local_addr().unwrap().port(),
            root.path().join("missing.exe"),
        )
        .await
        .unwrap();
        assert!(!backend.authenticated);
        assert_eq!(backend.updated_at, 123);
        assert!(backend.session.is_none());
        assert!(timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn unowned_stop_is_a_noop_without_network() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut backend = Backend::new(
            storage.clone(),
            listener.local_addr().unwrap().port(),
            root.path().join("missing.exe"),
        )
        .await
        .unwrap();
        backend.stop_game(42).await.unwrap();
        storage
            .write("run.json", &json!({"ownedGameId":42}))
            .await
            .unwrap();
        backend.unowned_game_id = Some(42);
        backend.stop_game(42).await.unwrap();
        assert!(timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn membership_refresh_requires_an_explicit_connection() {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut backend = Backend::new(
            Storage::new(root.path().to_owned()).unwrap(),
            listener.local_addr().unwrap().port(),
            root.path().join("unused.exe"),
        )
        .await
        .unwrap();
        assert!(backend.refresh_membership().await.is_err());
        assert_eq!(backend.membership, crate::membership::unavailable());
        assert!(timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn membership_reads_expiry_and_clears_it_on_logout_or_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            for (index, value) in [
                json!({"loggedIn":true,"expiresAt":1794499199000_i64,"memberStatus":1}),
                json!({"loggedIn":false,"expiresAt":1794499199000_i64,"memberStatus":1}),
            ]
            .into_iter()
            .enumerate()
            {
                let request: Value =
                    serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                        .unwrap();
                let expression = text(&request["params"]["expression"]);
                assert!(expression.contains(crate::membership::READ_USER_MEMBERSHIP));
                assert_eq!(expression.contains("update-user-info"), index == 0);
                ws.send(Message::Text(
                    json!({
                        "id":request["id"], "result":{"result":{"type":"object","value":value}}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            }
            let _ = ws.next().await;
            ws.close(None).await.unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let mut backend = Backend::new(
            Storage::new(root.path().to_owned()).unwrap(),
            port,
            root.path().join("unused.exe"),
        )
        .await
        .unwrap();
        backend.cdp = Some(
            Cdp::connect(
                &format!("ws://127.0.0.1:{port}/devtools/page/fixture"),
                port,
            )
            .await
            .unwrap(),
        );
        backend.connection = json!({"connected":true,"loggedIn":true});
        let membership = backend.refresh_membership().await.unwrap();
        assert_eq!(membership["expiresAt"], 1794499199000_i64);
        assert_eq!(membership["kind"], "full");
        assert!(membership["checkedAt"].as_i64().unwrap() > 0);
        backend.authenticated = true;
        assert!(!backend.logged_in().await.unwrap());
        assert_eq!(backend.membership, crate::membership::unavailable());
        assert!(!backend.authenticated);
        backend.membership = membership;
        assert!(backend.logged_in().await.is_err());
        assert_eq!(backend.membership, crate::membership::unavailable());
        assert_eq!(backend.connection["connected"], false);
        server.await.unwrap();
        assert!(!root.path().join("membership.json").exists());
    }

    #[tokio::test]
    async fn http_redirect_is_not_followed() {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = source.local_addr().unwrap();
        let redirect = destination.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut buffer = [0; 4096];
            socket.read(&mut buffer).await.unwrap();
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{redirect}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let response = http_client(Duration::from_secs(2))
            .unwrap()
            .get(format!("http://{address}/"))
            .header(COOKIE, "fixture=private")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 302);
        assert!(timeout(Duration::from_millis(50), destination.accept())
            .await
            .is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn status_reads_idle_starting_success_failure_and_reset_through_cdp() {
        let snapshots = [
            json!({"bytes":0,"msg":""}),
            json!({"bytes":0,"msg":"","checkSpeedUpInfo":{"canSpeedup":true}}),
            json!({"bytes":1024,"code":0,"msg":"","gameId":42,"area":{"areaId":7}}),
            json!({"bytes":0,"code":200115,"msg":"节点暂时不可用"}),
            json!({"bytes":0,"msg":""}),
        ];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            for raw in snapshots {
                let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["method"], "Runtime.evaluate");
                assert!(text(&request["params"]["expression"]).contains("get-accelerate-data"));
                ws.send(Message::Text(
                    json!({
                        "id":request["id"],
                        "result":{"result":{"type":"object","value":raw}}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let mut backend = Backend::new(
            Storage::new(root.path().to_owned()).unwrap(),
            port,
            root.path().join("unused.exe"),
        )
        .await
        .unwrap();
        backend.cdp = Some(
            Cdp::connect(
                &format!("ws://127.0.0.1:{port}/devtools/page/fixture"),
                port,
            )
            .await
            .unwrap(),
        );

        for (active, code, game_id) in [
            (false, Value::Null, Value::Null),
            (false, Value::Null, Value::Null),
            (true, json!(0), json!(42)),
            (false, json!(200115), Value::Null),
            (false, Value::Null, Value::Null),
        ] {
            let status = backend.status().await.unwrap();
            assert_eq!(status["active"], active);
            assert_eq!(status["code"], code);
            assert_eq!(status["gameId"], game_id);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cdp_matches_ids_ignores_events_and_preserves_json_values() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let request = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            ws.send(Message::Text(
                json!({"method":"Runtime.consoleAPICalled","params":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(json!({"id":request["id"],"result":{"result":{"type":"object","value":{"ok":true,"id":"001"}}}}).to_string().into())).await.unwrap();
        });
        let mut cdp = Cdp::connect(&format!("ws://127.0.0.1:{port}/devtools/page/test"), port)
            .await
            .unwrap();
        assert_eq!(
            cdp.evaluate("fixture()").await.unwrap(),
            json!({"ok":true,"id":"001"})
        );
        server.await.unwrap();
    }
}
