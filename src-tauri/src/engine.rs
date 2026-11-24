use crate::{
    backend::{ApiError, Backend},
    domain::{build_plan, day_key, now_ms, valid_interval},
    entitlement::FreeAccess,
    lease::Lease,
    storage::Storage,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU16, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, Notify},
    task::JoinHandle,
};

#[async_trait]
pub(crate) trait Service: Send {
    fn snapshot(&self) -> Value;
    async fn action(&mut self, action: &str, payload: &Value) -> Result<Value, String>;
    async fn connect_session(&mut self, id: &str) -> Result<(), String>;
    async fn refresh(&mut self, id: &str) -> Result<Value, String>;
    async fn status(&mut self) -> Result<Value, String>;
    async fn refresh_membership(&mut self) -> Result<Value, String>;
    async fn free_catalog(&mut self) -> Result<HashSet<i64>, String>;
    async fn free_access(
        &mut self,
        id: i64,
        platform: i64,
        in_free_catalog: bool,
    ) -> Result<FreeAccess, String>;
    async fn start_game(
        &mut self,
        id: i64,
        area: Option<i64>,
        platform: i64,
    ) -> Result<Value, String>;
    async fn stop_game(&mut self, id: i64) -> Result<(), String>;
    async fn claim(&mut self, id: &str, task: &Value) -> Result<Value, ApiError>;
    async fn draw(&mut self, id: &str) -> Result<Value, ApiError>;
    async fn prize(&mut self, id: &str, inventory: &Value) -> Result<Value, String>;
}

#[async_trait]
impl Service for Backend {
    fn snapshot(&self) -> Value {
        json!({"connection":self.connection,"membership":self.membership,"activities":self.activities,"mappings":self.mappings,
            "authenticated":self.authenticated,"updatedAt":self.updated_at,"clientSettings":self.client_settings})
    }
    async fn action(&mut self, action: &str, payload: &Value) -> Result<Value, String> {
        match action {
            "connect" => self.connect().await,
            "membership" => self.refresh_membership().await,
            "client-settings" => self.save_client_settings(payload).await,
            "scan" => {
                self.scan(payload["publicOnly"].as_bool().unwrap_or(false))
                    .await
            }
            "import" => {
                self.add_url(text(payload, "url")?).await?;
                Ok(json!({"ok":true}))
            }
            "search" => {
                let platform = payload
                    .get("platformId")
                    .filter(|value| !value.is_null())
                    .map_or(Ok(crate::domain::PC_PLATFORM), |value| {
                        value.as_i64().ok_or("游戏搜索平台无效")
                    })?;
                Ok(json!(
                    self.search(text(payload, "keyword")?, platform).await?
                ))
            }
            "mapping" => {
                self.set_mapping(payload).await?;
                Ok(json!({"ok":true}))
            }
            "open" => {
                self.open_activity(text(payload, "id")?).await?;
                Ok(json!({"ok":true}))
            }
            _ => Err("操作不存在".to_owned()),
        }
    }
    async fn connect_session(&mut self, id: &str) -> Result<(), String> {
        Backend::connect_session(self, id).await
    }
    async fn refresh(&mut self, id: &str) -> Result<Value, String> {
        Backend::refresh(self, id).await
    }
    async fn status(&mut self) -> Result<Value, String> {
        Backend::status(self).await
    }
    async fn refresh_membership(&mut self) -> Result<Value, String> {
        Backend::refresh_membership(self).await
    }
    async fn free_catalog(&mut self) -> Result<HashSet<i64>, String> {
        Backend::free_catalog(self).await
    }
    async fn free_access(
        &mut self,
        id: i64,
        platform: i64,
        in_free_catalog: bool,
    ) -> Result<FreeAccess, String> {
        Backend::free_access(self, id, platform, in_free_catalog).await
    }
    async fn start_game(
        &mut self,
        id: i64,
        area: Option<i64>,
        platform: i64,
    ) -> Result<Value, String> {
        Backend::start_game(self, id, area, platform).await
    }
    async fn stop_game(&mut self, id: i64) -> Result<(), String> {
        Backend::stop_game(self, id).await
    }
    async fn claim(&mut self, id: &str, task: &Value) -> Result<Value, ApiError> {
        Backend::claim(self, id, task).await
    }
    async fn draw(&mut self, id: &str) -> Result<Value, ApiError> {
        Backend::draw(self, id).await
    }
    async fn prize(&mut self, id: &str, inventory: &Value) -> Result<Value, String> {
        Backend::prize(self, id, inventory).await
    }
}

pub struct Engine {
    service: Mutex<Box<dyn Service>>,
    cached: StdMutex<Value>,
    run: Mutex<Value>,
    gate: Mutex<()>,
    operation: StdMutex<Option<String>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    pub storage: Storage,
    pub busy: AtomicBool,
    pub closing: AtomicBool,
    cancelled: AtomicBool,
    wake: Notify,
    port: AtomicU16,
    tick: Duration,
    buffer: f64,
}

#[derive(Clone, Copy)]
enum QueueAccess {
    Member(i64),
    Free(FreeAccess),
}

impl QueueAccess {
    fn kind(self, now: i64) -> Option<&'static str> {
        match self {
            Self::Member(expires_at) => (expires_at > now).then_some("member"),
            Self::Free(access) => access.kind(now),
        }
    }
}

struct Operation<'a>(&'a StdMutex<Option<String>>);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

impl Engine {
    pub async fn new(
        service: Box<dyn Service>,
        storage: Storage,
        port: u16,
    ) -> Result<Arc<Self>, String> {
        let saved = storage.read("run.json", json!({})).await?;
        let run = restore_run(saved)?;
        Ok(Arc::new(Self {
            cached: StdMutex::new(service.snapshot()),
            service: Mutex::new(service),
            run: Mutex::new(run),
            gate: Mutex::new(()),
            operation: StdMutex::new(None),
            worker: Mutex::new(None),
            storage,
            busy: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            wake: Notify::new(),
            port: AtomicU16::new(port),
            tick: Duration::from_secs(5),
            buffer: 20.0,
        }))
    }
    pub async fn snapshot(&self) -> Value {
        // 状态读取独立于网络锁，连接或领奖时仍可操作“停止”。
        let mut value = self.cached.lock().unwrap().clone();
        value["plan"] = build_plan(
            value["activities"].as_array().unwrap_or(&Vec::new()),
            &value["mappings"],
            None,
            now_ms(),
        );
        value["operation"] = json!(*self.operation.lock().unwrap());
        value["run"] = self.run.lock().await.clone();
        value["dataDir"] = json!(self.storage.dir().to_string_lossy());
        value["version"] = json!(env!("CARGO_PKG_VERSION"));
        value
    }
    pub fn has_operation(&self) -> bool {
        self.operation.lock().unwrap().is_some()
    }
    fn publish(&self, service: &dyn Service) {
        *self.cached.lock().unwrap() = service.snapshot();
    }
    async fn change(&self, update: impl FnOnce(&mut Value)) -> Result<(), String> {
        let mut run = self.run.lock().await;
        let mut next = run.clone();
        update(&mut next);
        // 写入失败时保留原内存状态，尤其不能提前忘记 pending 或加速归属。
        self.storage.write("run.json", &next).await?;
        *run = next;
        Ok(())
    }
    async fn log(&self, message: impl Into<String>, level: &str) -> Result<(), String> {
        let event = json!({"time":now_ms(),"level":level,"message":message.into()});
        let mut run = self.run.lock().await;
        let logs = run["logs"].as_array_mut().ok_or("运行日志格式无效")?;
        logs.push(event.clone());
        if logs.len() > 200 {
            logs.drain(..logs.len() - 200);
        }
        self.storage.log(&event).await?;
        self.storage.write("run.json", &run).await
    }
    async fn assert_running(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err("用户停止运行".to_owned());
        }
        if self.run.lock().await["day"] != day_key(now_ms()) {
            return Err("已跨过活动每日重置时间，请刷新活动后重新开始".to_owned());
        }
        Ok(())
    }
    pub async fn dispatch(self: &Arc<Self>, action: &str, payload: Value) -> Result<Value, String> {
        if !payload.is_object() || payload.to_string().len() > 100_000 {
            return Err("请求内容无效或过大".to_owned());
        }
        if action == "stop" {
            self.stop().await?;
            return Ok(json!({"ok":true}));
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err("工具正在退出".to_owned());
        }
        let _gate = self
            .gate
            .try_lock()
            .map_err(|_| "正在处理请求，请稍候".to_owned())?;
        if self.busy.load(Ordering::SeqCst) {
            return Err("请先停止任务，再修改配置或刷新活动".to_owned());
        }
        *self.operation.lock().unwrap() = Some(action.to_owned());
        let _operation = Operation(&self.operation);
        match action {
            "start" => {
                let ids: Vec<String> = serde_json::from_value(payload["ids"].clone())
                    .map_err(|_| "活动编号格式无效".to_owned())?;
                let cache = self.cached.lock().unwrap().clone();
                let entries = cache["activities"].as_array().ok_or("活动缓存格式无效")?;
                if ids.is_empty()
                    || ids.len() > 500
                    || ids.iter().any(|id| !entries.iter().any(|a| a["id"] == *id))
                {
                    return Err("请选择有效活动".to_owned());
                }
                let mut unique = ids.clone();
                unique.sort();
                unique.dedup();
                if ids.len() != unique.len() {
                    return Err("活动编号重复".to_owned());
                }
                let run = self.run.lock().await;
                if !run["pending"].is_null() {
                    return Err("有操作结果待核对，请先核对记录".to_owned());
                }
                if !run["ownedGameId"].is_null() {
                    return Err("请先停止上次遗留的加速".to_owned());
                }
                drop(run);
                let lease = Lease::acquire(self.port.load(Ordering::SeqCst))?;
                self.cancelled.store(false, Ordering::SeqCst);
                self.change(|r| {
                    r["status"] = json!("starting");
                    r["day"] = json!(day_key(now_ms()));
                    r["message"] = json!("正在读取服务器状态");
                    r["selectedIds"] = json!(ids);
                    r["startedAt"] = json!(now_ms());
                    r["group"] = Value::Null;
                    r["elapsed"] = json!(0);
                })
                .await?;
                self.busy.store(true, Ordering::SeqCst);
                let engine = Arc::clone(self);
                *self.worker.lock().await = Some(tokio::spawn(async move {
                    engine.run_worker(ids).await;
                    drop(lease);
                    engine.busy.store(false, Ordering::SeqCst);
                }));
                Ok(json!({"ok":true}))
            }
            "reconcile" => {
                if payload["confirmed"] != true {
                    return Err("请先在官方页面核对操作结果".to_owned());
                }
                self.change(|r| r["pending"] = Value::Null).await?;
                self.log("用户已核对上次操作结果，允许重新读取服务器状态", "info")
                    .await?;
                Ok(json!({"ok":true}))
            }
            "stop-owned" => {
                if self.run.lock().await["ownedGameId"].is_null() {
                    return Err("没有记录到由工具启动的加速".to_owned());
                }
                self.stop_owned().await?;
                Ok(json!({"ok":true}))
            }
            _ => {
                if action == "client-settings" {
                    let run = self.run.lock().await;
                    if !run["pending"].is_null() || !run["ownedGameId"].is_null() {
                        return Err("请先核对待确认记录并停止遗留加速，再修改客户端设置".into());
                    }
                }
                if action == "connect" {
                    self.log("开始连接 biubiu 客户端", "info").await?;
                }
                let mut service = self.service.lock().await;
                let result = service.action(action, &payload).await;
                if action == "client-settings" && result.is_ok() {
                    let port = service.snapshot()["clientSettings"]["debugPort"]
                        .as_u64()
                        .and_then(|p| u16::try_from(p).ok())
                        .filter(|p| *p > 0)
                        .ok_or("客户端设置未返回有效端口")?;
                    // 配置保存与下一次任务租约使用同一端口，整个更新仍在 gate 内。
                    self.port.store(port, Ordering::SeqCst);
                }
                self.publish(service.as_ref());
                drop(service);
                if action == "connect" {
                    let (message, level) = match &result {
                        Ok(_) => ("biubiu 客户端连接成功".to_owned(), "info"),
                        Err(error) => (format!("连接 biubiu 客户端失败：{error}"), "error"),
                    };
                    if let Err(log_error) = self.log(message, level).await {
                        return Err(match result {
                            Ok(_) => format!("客户端已连接，但连接日志写入失败：{log_error}"),
                            Err(error) => format!("{error}；连接日志写入失败：{log_error}"),
                        });
                    }
                }
                result
            }
        }
    }
    pub async fn stop(&self) -> Result<(), String> {
        if self.busy.load(Ordering::SeqCst) {
            self.cancelled.store(true, Ordering::SeqCst);
            self.wake.notify_one();
            self.change(|r| {
                if matches!(r["status"].as_str(), Some("starting" | "running")) {
                    r["status"] = json!("stopping");
                }
            })
            .await?;
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<(), String> {
        self.closing.store(true, Ordering::SeqCst);
        let mut failure = self.stop().await.err();
        let _gate = self.gate.lock().await;
        // 即使状态落盘失败，也要等待已发出的请求并尝试停止自有加速。
        if let Err(error) = self.stop().await {
            failure.get_or_insert(error);
        }
        if let Some(worker) = self.worker.lock().await.take() {
            if let Err(error) = worker.await {
                failure.get_or_insert(format!("任务队列异常：{error}"));
            }
        }
        if let Err(error) = self.stop_owned().await {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.change(|_| {}).await {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }
    async fn stop_owned(&self) -> Result<(), String> {
        let id = self.run.lock().await["ownedGameId"].as_i64();
        if let Some(id) = id {
            let result = self.service.lock().await.stop_game(id).await;
            match result {
                Ok(()) => {
                    self.change(|r| {
                        r["ownedGameId"] = Value::Null;
                        r["cleanupError"] = Value::Null;
                    })
                    .await?
                }
                Err(error) => {
                    self.change(|r| {
                        r["cleanupError"] = json!(error);
                        r["status"] = json!("error");
                        r["message"] = json!(format!("请检查客户端加速状态：{error}"));
                    })
                    .await?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    async fn refresh(&self, id: &str) -> Result<Value, String> {
        let mut service = self.service.lock().await;
        let result = service.refresh(id).await;
        self.publish(service.as_ref());
        result
    }
    async fn run_worker(&self, ids: Vec<String>) {
        let result = self.prepare_execute(&ids).await;
        if let Err(error) = result {
            let status = if self.cancelled.load(Ordering::SeqCst) {
                "stopped"
            } else {
                "error"
            };
            let _ = self
                .change(|r| {
                    r["status"] = json!(status);
                    r["message"] = json!(error);
                })
                .await;
            if let Err(error) = self.log(error, "error").await {
                self.run.lock().await["message"] = json!(format!("保存执行状态失败：{error}"));
            }
        }
        if let Err(error) = self.stop_owned().await {
            let mut run = self.run.lock().await;
            run["status"] = json!("error");
            run["message"] = json!(format!("停止或保存加速状态失败：{error}"));
        }
    }
    async fn prepare_execute(&self, ids: &[String]) -> Result<(), String> {
        self.assert_running().await?;
        {
            let mut service = self.service.lock().await;
            service.connect_session(&ids[0]).await?;
            // 启动流程绕过 action 分发，连接状态也要立即进入桌面快照。
            self.publish(service.as_ref());
        }
        self.assert_running().await?;
        let current = self.service.lock().await.status().await?;
        if current["active"] == true {
            return Err(format!(
                "客户端正在加速 {}，请先停止后再运行任务队列",
                current["gameName"].as_str().unwrap_or("游戏")
            ));
        }
        for id in ids {
            self.assert_running().await?;
            self.refresh(id).await?;
        }
        let cache = self.cached.lock().unwrap().clone();
        let activities = cache["activities"].as_array().ok_or("活动列表无效")?;
        let plan = build_plan(activities, &cache["mappings"], Some(ids), now_ms());
        let groups = plan["groups"].as_array().ok_or("任务计划无效")?;
        if groups.is_empty()
            && !activities.iter().any(|a| {
                ids.iter().any(|id| a["id"] == *id) && a["tickets"].as_f64().unwrap_or(0.0) > 0.0
            })
        {
            return Err("所选活动没有可运行任务或可用抽奖次数".to_owned());
        }
        self.assert_running().await?;
        self.change(|r| {
            r["status"] = json!("running");
            r["plan"] = plan.clone();
            r["message"] = json!("正在执行");
        })
        .await?;
        self.log(format!("开始执行 {} 组游戏任务", groups.len()), "info")
            .await?;
        // 已经可以领取的任务和现有抽奖次数不需要加速，也可能发放会员时长。
        let mut pending = Vec::new();
        for group in groups {
            let tasks = self.prepare_group(group).await?;
            if !tasks.is_empty() {
                let mut group = group.clone();
                group["tasks"] = json!(tasks);
                pending.push(group);
            }
        }
        for id in ids {
            self.assert_running().await?;
            self.draw_tickets(id, None).await?;
        }
        self.execute_pending(&mut pending).await?;
        self.assert_running().await?;
        let message = if pending.is_empty() {
            "本轮执行结束，请查看任务与奖品记录".to_owned()
        } else {
            format!(
                "本轮执行结束：未确认有效会员时长，{} 组未确认免费可用的任务暂未执行",
                pending.len()
            )
        };
        self.change(|r| {
            r["status"] = json!("completed");
            r["group"] = Value::Null;
            r["message"] = json!(message);
        })
        .await?;
        self.log(message, if pending.is_empty() { "info" } else { "warn" })
            .await
    }

    async fn queue_membership(&self) -> Result<Option<i64>, String> {
        self.assert_running().await?;
        let result = {
            let mut service = self.service.lock().await;
            let result = service.refresh_membership().await;
            self.publish(service.as_ref());
            result
        };
        self.assert_running().await?;
        match result {
            Ok(snapshot) => Ok(snapshot["expiresAt"].as_i64().filter(|end| *end > now_ms())),
            Err(error) => {
                self.log(
                    format!("会员状态刷新失败，未确认有效会员时长：{error}"),
                    "warn",
                )
                .await?;
                Ok(None)
            }
        }
    }

    async fn execute_pending(&self, pending: &mut Vec<Value>) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let mut membership = self.queue_membership().await?;
        let mut catalog = None;
        let mut access_by_game = HashMap::new();
        while !pending.is_empty() {
            self.assert_running().await?;
            if membership.is_some_and(|end| end <= now_ms()) {
                // 上一组可能已获得时长；旧的到期时间不能用来判断续期结果。
                membership = self.queue_membership().await?;
            }
            let choice = if let Some(end) = membership.filter(|end| *end > now_ms()) {
                Some((0, QueueAccess::Member(end)))
            } else {
                let catalog = match &catalog {
                    Some(ids) => ids,
                    None => {
                        let result = self.service.lock().await.free_catalog().await;
                        let ids = match result {
                            Ok(ids) => ids,
                            Err(error) => {
                                self.log(
                                    format!(
                                        "免费游戏库读取失败，仅尝试有明确限免时段的游戏：{error}"
                                    ),
                                    "warn",
                                )
                                .await?;
                                HashSet::new()
                            }
                        };
                        catalog.insert(ids)
                    }
                };
                for group in pending.iter() {
                    self.assert_running().await?;
                    let (id, platform) = group_game(group)?;
                    if let std::collections::hash_map::Entry::Vacant(entry) =
                        access_by_game.entry((id, platform))
                    {
                        let result = self
                            .service
                            .lock()
                            .await
                            .free_access(id, platform, catalog.contains(&id))
                            .await;
                        let access = match result {
                            Ok(access) => access,
                            Err(error) => {
                                self.log(
                                    format!(
                                        "无法确认 {} 的免费资格：{error}",
                                        group["gameName"].as_str().unwrap_or("游戏")
                                    ),
                                    "warn",
                                )
                                .await?;
                                FreeAccess::Unconfirmed
                            }
                        };
                        entry.insert(access);
                    }
                }
                let now = now_ms();
                let mut free = None;
                let mut limited = None;
                for (index, group) in pending.iter().enumerate() {
                    let access = access_by_game[&group_game(group)?];
                    match access.kind(now) {
                        Some("limited-free") => {
                            limited = Some((index, QueueAccess::Free(access)));
                            break;
                        }
                        Some("free-catalog") if free.is_none() => {
                            free = Some((index, QueueAccess::Free(access)))
                        }
                        _ => {}
                    }
                }
                limited.or(free)
            };
            let Some((index, access)) = choice else { break };
            let group = &pending[index];
            let tasks = self.prepare_group(group).await?;
            if !tasks.is_empty() {
                let Some(kind) = access.kind(now_ms()) else {
                    // 刷新任务状态期间权益可能已经到期，重新排队而不是强行启动。
                    pending[index]["tasks"] = json!(tasks);
                    continue;
                };
                self.execute_group(group, tasks, kind).await?;
            }
            pending.remove(index);
            if matches!(access, QueueAccess::Free(_)) && !pending.is_empty() {
                let renewed = self.queue_membership().await?;
                if renewed.is_some() {
                    self.log("已确认账号有新的有效会员时长，继续执行剩余任务", "info")
                        .await?;
                }
                membership = renewed;
            }
        }
        for group in pending {
            self.log(
                format!(
                    "{} 暂未执行：无已确认有效的会员时长或免费资格",
                    group["gameName"].as_str().unwrap_or("游戏")
                ),
                "warn",
            )
            .await?;
        }
        Ok(())
    }

    async fn prepare_group(&self, group: &Value) -> Result<Vec<Value>, String> {
        let mut pending = Vec::new();
        for task in group["tasks"].as_array().ok_or("任务组格式无效")? {
            self.assert_running().await?;
            let id = text(task, "activityId")?;
            let activity = self.refresh(id).await?;
            let fresh = activity["tasks"]
                .as_array()
                .and_then(|ts| ts.iter().find(|t| t["id"] == task["taskId"]));
            match fresh {
                Some(t)
                    if crate::domain::mobile_task_reason(t).is_some()
                        || crate::domain::mobile_task_reason(task).is_some() =>
                {
                    self.log(
                        format!("跳过手机端任务：{}", t["title"].as_str().unwrap_or("")),
                        "warn",
                    )
                    .await?
                }
                Some(t) if t["status"] == 3 => self.draw_tickets(id, Some(activity)).await?,
                Some(t) if t["status"] == 2 => self.collect(task).await?,
                Some(t)
                    if t["status"] == 1 && (t["supported"] == true || task["manual"] == true) =>
                {
                    pending.push(task.clone())
                }
                _ => {
                    self.log(
                        format!("任务状态已变化：{}", task["title"].as_str().unwrap_or("")),
                        "warn",
                    )
                    .await?
                }
            }
        }
        Ok(pending)
    }

    async fn execute_group(
        &self,
        group: &Value,
        pending: Vec<Value>,
        kind: &str,
    ) -> Result<(), String> {
        self.assert_running().await?;
        let (id, _) = group_game(group)?;
        self.change(|r| {
            let mut active = group.clone();
            active["tasks"] = json!(pending);
            active["access"] = json!(kind);
            r["group"] = active;
            r["elapsed"] = json!(0);
            r["ownedGameId"] = json!(id);
        })
        .await?;
        let result = self.accelerate(group, pending, kind).await;
        // 无论开始、领取还是计时失败，都仅清理本工具记录的游戏。
        self.stop_owned().await?;
        if let Err(error) = result {
            if !self.run.lock().await["pending"].is_null() || self.assert_running().await.is_err() {
                return Err(error);
            }
            self.log(
                format!(
                    "{} 已跳过：{error}",
                    group["gameName"].as_str().unwrap_or("游戏")
                ),
                "warn",
            )
            .await?;
        }
        Ok(())
    }
    async fn accelerate(
        &self,
        group: &Value,
        mut pending: Vec<Value>,
        kind: &str,
    ) -> Result<(), String> {
        self.assert_running().await?;
        let (id, platform) = group_game(group)?;
        let category = match kind {
            "limited-free" => "限免时段",
            "free-catalog" => "免费目录",
            _ => "会员有效",
        };
        self.log(
            format!(
                "开始加速 {}（{} · {category}）",
                group["gameName"].as_str().unwrap_or("游戏"),
                crate::domain::platform_name(platform)
            ),
            "info",
        )
        .await?;
        let mut previous = self
            .service
            .lock()
            .await
            .start_game(id, group["areaId"].as_i64(), platform)
            .await?;
        let area = group["areaId"]
            .as_i64()
            .or_else(|| previous["areaId"].as_i64());
        if !crate::domain::acceleration_matches(&previous, id, platform, area) {
            return Err("客户端返回的加速游戏、平台或区服与任务不符".to_owned());
        }
        let mut wall = now_ms();
        let mut mono = Instant::now();
        let mut elapsed = 0.0;
        while !pending.is_empty() {
            self.assert_running().await?;
            tokio::select! { _ = tokio::time::sleep(self.tick) => {}, _ = self.wake.notified() => {} }
            self.assert_running().await?;
            let current = self.service.lock().await.status().await?;
            let next_wall = now_ms();
            let next_mono = Instant::now();
            if !crate::domain::acceleration_matches(&current, id, platform, area) {
                return Err("加速中断或游戏、平台、区服已切换，停止累计时间".to_owned());
            }
            elapsed += valid_interval(
                &previous,
                &current,
                (next_wall - wall) as f64,
                next_mono.duration_since(mono).as_secs_f64() * 1000.0,
                id,
                15000.0_f64.max(self.tick.as_secs_f64() * 2000.0),
            );
            wall = next_wall;
            mono = next_mono;
            previous = current;
            self.change(|r| r["elapsed"] = json!(elapsed)).await?;
            for i in (0..pending.len()).rev() {
                if elapsed < pending[i]["seconds"].as_f64().ok_or("任务时长无效")? + self.buffer
                {
                    continue;
                }
                if let Err(error) = self.collect(&pending[i]).await {
                    if !self.run.lock().await["pending"].is_null()
                        || self.assert_running().await.is_err()
                    {
                        return Err(error);
                    }
                    self.log(
                        format!(
                            "任务领取失败：{}；{error}",
                            pending[i]["title"].as_str().unwrap_or("")
                        ),
                        "warn",
                    )
                    .await?;
                }
                pending.remove(i);
            }
        }
        Ok(())
    }
    async fn mutation(&self, kind: &str, ids: Value) -> Result<Value, String> {
        self.assert_running().await?;
        let mut marker = ids.clone();
        marker["kind"] = json!(kind);
        marker["startedAt"] = json!(now_ms());
        {
            let mut run = self.run.lock().await;
            if !run["pending"].is_null() {
                return Err("存在结果待核对的操作，请先核对记录".to_owned());
            }
            run["pending"] = marker.clone();
            self.storage.write("run.json", &run).await?;
        }
        let id = text(&ids, "activityId")?;
        let mut service = self.service.lock().await;
        let result = if kind == "claim" {
            service.claim(id, &ids["taskId"]).await
        } else {
            service.draw(id).await
        };
        drop(service);
        let mut run = self.run.lock().await;
        match result {
            Ok(value) => {
                if kind == "draw" {
                    let mut record = ids;
                    record["time"] = json!(now_ms());
                    record["inventoryDetailId"] = value["inventoryDetailId"].clone();
                    record["prizeName"] = json!(prize_name(&value));
                    record["result"] = value.clone();
                    run["results"]
                        .as_array_mut()
                        .ok_or("奖品记录格式无效")?
                        .push(record);
                }
                // HTTP 成功不等于状态核验完成；后续详情、日志或刷新失败时仍须保留标记。
                // collect/draw_tickets 核验服务器状态后才清除此标记。
                self.storage.write("run.json", &run).await?;
                Ok(value)
            }
            Err(error) => {
                if error.definitive {
                    let mut next = run.clone();
                    next["pending"] = Value::Null;
                    self.storage.write("run.json", &next).await?;
                    *run = next;
                }
                Err(error.message)
            }
        }
    }
    async fn collect(&self, task: &Value) -> Result<(), String> {
        self.assert_running().await?;
        if let Some(reason) = crate::domain::mobile_task_reason(task) {
            return Err(reason.to_owned());
        }
        let id = text(task, "activityId")?;
        let mut activity = self.refresh(id).await?;
        check_activity(&activity)?;
        let current = activity["tasks"]
            .as_array()
            .and_then(|ts| ts.iter().find(|t| t["id"] == task["taskId"]))
            .cloned()
            .ok_or("任务已更新，请重新扫描")?;
        if let Some(reason) = crate::domain::mobile_task_reason(&current) {
            return Err(reason.to_owned());
        }
        if current["status"] != 3 {
            // 与旧 runner 一致，计时达标后允许报告仍处于 status=1 的任务；
            // 部分活动只有 reportTaskFinish 后才更新状态，不能强制等待 status=2。
            if current["status"] != 1 && current["status"] != 2 {
                return Err("任务状态已变化，请重新扫描".to_owned());
            }
            self.mutation("claim", json!({"activityId":id,"taskId":task["taskId"]}))
                .await?;
            activity = self.refresh(id).await?;
            let confirmed = activity["tasks"]
                .as_array()
                .and_then(|ts| ts.iter().find(|t| t["id"] == task["taskId"]));
            if !claim_confirmed(confirmed, &current, &activity) {
                self.change(|r| r["pending"] = json!({"kind":"claim-reconcile","activityId":id,"taskId":task["taskId"],"startedAt":now_ms()})).await?;
                return Err("领取返回后任务状态未更新，已暂停，请到官方页面核对".to_owned());
            }
            self.change(|r| r["pending"] = Value::Null).await?;
            self.log(
                format!(
                    "已领取：{} / {}",
                    task["activityName"].as_str().unwrap_or(id),
                    task["title"].as_str().unwrap_or("")
                ),
                "info",
            )
            .await?;
        }
        self.draw_tickets(id, Some(activity)).await
    }
    async fn draw_tickets(&self, id: &str, initial: Option<Value>) -> Result<(), String> {
        self.assert_running().await?;
        let mut activity = match initial {
            Some(a) => a,
            None => self.refresh(id).await?,
        };
        let budget = tickets(&activity)?.min(50);
        for _ in 0..budget {
            self.assert_running().await?;
            check_activity(&activity)?;
            let before = tickets(&activity)?;
            if before == 0 {
                break;
            }
            let draw = self
                .mutation(
                    "draw",
                    json!({"activityId":id,"activityName":activity["name"],"beforeTickets":before}),
                )
                .await?;
            if !draw["inventoryDetailId"].is_null() {
                tokio::time::sleep(Duration::from_millis(1200)).await;
                let result = self
                    .service
                    .lock()
                    .await
                    .prize(id, &draw["inventoryDetailId"])
                    .await;
                match result {
                    Ok(detail) => {
                        self.change(|r| {
                            if let Some(record) =
                                r["results"].as_array_mut().and_then(|rs| rs.last_mut())
                            {
                                if !prize_name(&detail).is_empty() {
                                    record["prizeName"] = json!(prize_name(&detail));
                                }
                                record["detail"] = detail;
                            }
                        })
                        .await?
                    }
                    Err(_) => {
                        self.log(
                            "抽奖成功，奖品详情暂未返回，可到活动页“我的奖励”查看",
                            "warn",
                        )
                        .await?
                    }
                }
            }
            self.log(
                format!("完成抽奖：{}", activity["name"].as_str().unwrap_or(id)),
                "info",
            )
            .await?;
            // 刷新失败与次数未下降同样保守处理，避免下次开始重复花费未知次数。
            let refreshed = self.refresh(id).await;
            let verified = refreshed
                .as_ref()
                .ok()
                .and_then(|a| tickets(a).ok())
                .is_some_and(|n| n < before);
            if !verified {
                self.change(|r| r["pending"] = json!({"kind":"draw-reconcile","activityId":id,"beforeTickets":before,
                    "inventoryDetailId":draw["inventoryDetailId"],"startedAt":now_ms()})).await?;
                return Err(
                    "抽奖返回后剩余次数未确认，已暂停，请在官方页面核对奖品和次数".to_owned(),
                );
            }
            self.change(|r| r["pending"] = Value::Null).await?;
            activity = refreshed?;
        }
        Ok(())
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("参数 {key} 无效"))
}
fn group_game(group: &Value) -> Result<(i64, i64), String> {
    let id = group["gameId"].as_i64().ok_or("游戏编号无效")?;
    let platform = group["platformId"]
        .as_i64()
        .unwrap_or(crate::domain::PC_PLATFORM);
    Ok((id, platform))
}

fn tickets(activity: &Value) -> Result<u64, String> {
    activity["tickets"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= 1_000_000.0)
        .map(|n| n as u64)
        .ok_or_else(|| "抽奖次数格式异常，请在官方页面核对".to_owned())
}
fn check_activity(activity: &Value) -> Result<(), String> {
    // 与计划器共享日期转换，避免数字字符串在执行阶段绕过截止检查。
    let now = now_ms() as f64;
    if activity["isOver"] == true || crate::domain::js_number(activity.get("endTime")) <= now {
        return Err("活动已经结束".to_owned());
    }
    if crate::domain::js_number(activity.get("startTime")) > now {
        return Err("活动尚未开始".to_owned());
    }
    Ok(())
}
fn claim_confirmed(confirmed: Option<&Value>, previous: &Value, activity: &Value) -> bool {
    let Some(confirmed) = confirmed else {
        return false;
    };
    if confirmed["status"] == 3 {
        return true;
    }
    // 与旧 runner 保持一致：任务完成计数增长，或服务器已经发放抽奖次数，均视为已接受。
    // 字段缺失时 JavaScript 的 undefined 比较为 false，不能擅自当作零。
    let finish_not_increased = match (
        confirmed["finishCount"].as_f64(),
        previous["finishCount"].as_f64(),
    ) {
        (Some(next), Some(before)) => next <= before,
        _ => false,
    };
    let tickets_empty = activity["tickets"]
        .as_f64()
        .is_some_and(|tickets| tickets <= 0.0);
    !finish_not_increased || !tickets_empty
}

fn prize_name(value: &Value) -> &str {
    ["rightsName", "awardName", "name", "prizeName"]
        .iter()
        .find_map(|key| value[key].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("")
}
fn restore_run(saved: Value) -> Result<Value, String> {
    if !saved.is_object() {
        return Err("运行记录格式无效，请检查 run.json".to_owned());
    }
    let mut run = json!({"status":"idle","logs":[],"results":[],"pending":null,"group":null,"elapsed":0,
        "ownedGameId":null,"cleanupError":null});
    run.as_object_mut()
        .unwrap()
        .extend(saved.as_object().unwrap().clone());
    if !run["logs"].is_array() || !run["results"].is_array() {
        return Err("运行记录的日志或奖品格式无效".to_owned());
    }
    if matches!(
        run["status"].as_str(),
        Some("running" | "stopping" | "starting")
    ) {
        run["status"] = json!("interrupted");
        run["message"] = json!("上次运行中断；重新运行会核对服务器状态，并从零保守计时。");
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_dates_match_planner_numeric_string_semantics() {
        let now = now_ms();
        for string_dates in [false, true] {
            let date = |time: i64| {
                if string_dates {
                    json!(time.to_string())
                } else {
                    json!(time)
                }
            };
            assert!(check_activity(
                &json!({"startTime":date(now - 60_000),"endTime":date(now + 60_000)})
            )
            .is_ok());
            assert!(check_activity(
                &json!({"startTime":date(now + 60_000),"endTime":date(now + 120_000)})
            )
            .is_err());
            assert!(check_activity(&json!({"endTime":date(now - 60_000)})).is_err());
        }
        assert!(check_activity(&json!({})).is_ok());
        assert!(check_activity(&json!({"endTime":null})).is_err());
        assert!(check_activity(&json!({"endTime":"invalid date"})).is_ok());
        assert!(check_activity(&json!({"isOver":true})).is_err());
    }

    #[test]
    fn restores_saved_state_and_marks_interrupted_runs() {
        let saved = json!({
            "status":"running", "day":"2026-11-08", "pending":null,
            "ownedGameId":42, "logs":[], "results":[], "message":"执行中"
        });
        let restored = restore_run(saved).unwrap();
        assert_eq!(restored["status"], "interrupted");
        assert_eq!(restored["ownedGameId"], 42);
        assert_eq!(
            restored["message"],
            "上次运行中断；重新运行会核对服务器状态，并从零保守计时。"
        );

        let stopped = restore_run(json!({"status":"stopping","logs":[],"results":[]})).unwrap();
        assert_eq!(stopped["status"], "interrupted");
        let completed = restore_run(json!({"status":"completed","logs":[],"results":[]})).unwrap();
        assert_eq!(completed["status"], "completed");
    }

    #[test]
    fn claim_confirmation_keeps_ticket_increment_compatibility() {
        let previous = json!({"finishCount":1});
        let confirmed = json!({"status":2,"finishCount":1});
        assert!(claim_confirmed(
            Some(&confirmed),
            &previous,
            &json!({"tickets":1})
        ));
        assert!(!claim_confirmed(
            Some(&confirmed),
            &previous,
            &json!({"tickets":0})
        ));
        assert!(claim_confirmed(
            Some(&json!({"status":2,"finishCount":2})),
            &previous,
            &json!({"tickets":0})
        ));
        assert!(claim_confirmed(
            Some(&json!({"status":3,"finishCount":1})),
            &previous,
            &json!({"tickets":0})
        ));
        assert!(claim_confirmed(
            Some(&json!({"status":2})),
            &json!({}),
            &json!({"tickets":0})
        ));
        assert!(!claim_confirmed(None, &previous, &json!({"tickets":1})));
    }

    struct Fixture {
        activity: Value,
        mappings: Value,
        membership: Value,
        renew_on_draw: Option<i64>,
        catalog: HashSet<i64>,
        free_access: HashMap<i64, FreeAccess>,
        events: Vec<String>,
        starts: Vec<(i64, Option<i64>, i64)>,
        claims: Vec<(String, Value)>,
        acceleration: Value,
        claim_error: Option<ApiError>,
        claim_error_task: Option<Value>,
        draw_error: Option<ApiError>,
        refresh_after_claim_fails: bool,
        unchanged_tickets: bool,
        stop_failures: usize,
        active: bool,
        authenticated: bool,
    }

    struct Mock {
        data: StdMutex<Fixture>,
        hold_claim: AtomicBool,
        claim_entered: Notify,
        claim_release: Notify,
        hold_action: AtomicBool,
        action_entered: Notify,
        action_release: Notify,
    }

    impl Mock {
        fn new(status: i64, tickets: u64) -> Arc<Self> {
            Arc::new(Self {
                data: StdMutex::new(Fixture {
                    activity: json!({"id":"a","name":"测试活动","tickets":tickets,"tasks":[
                        {"id":1,"type":7,"status":status,"title":"测试任务","supported":true,"seconds":1,"finishCount":0}
                    ]}),
                    mappings: json!({"a:1":{"gameId":42,"gameName":"测试游戏"}}),
                    membership: json!({"expiresAt":now_ms() + 86_400_000}),
                    renew_on_draw: None,
                    catalog: HashSet::new(),
                    free_access: HashMap::new(),
                    events: Vec::new(),
                    starts: Vec::new(),
                    claims: Vec::new(),
                    acceleration: json!({"gameId":42,"gameName":"测试游戏"}),
                    claim_error: None,
                    claim_error_task: None,
                    draw_error: None,
                    refresh_after_claim_fails: false,
                    unchanged_tickets: false,
                    stop_failures: 0,
                    active: false,
                    authenticated: false,
                }),
                hold_claim: AtomicBool::new(false),
                claim_entered: Notify::new(),
                claim_release: Notify::new(),
                hold_action: AtomicBool::new(false),
                action_entered: Notify::new(),
                action_release: Notify::new(),
            })
        }
        fn events(&self) -> Vec<String> {
            self.data.lock().unwrap().events.clone()
        }
    }

    // 纯内存 Service：不创建调试连接，不调用真实领奖接口。
    struct MockService(Arc<Mock>);
    #[async_trait]
    impl Service for MockService {
        fn snapshot(&self) -> Value {
            let data = self.0.data.lock().unwrap();
            json!({"activities":[data.activity],"mappings":data.mappings,"membership":data.membership,
                "authenticated":data.authenticated,"connection":{"connected":data.authenticated,"loggedIn":data.authenticated}})
        }
        async fn action(&mut self, _action: &str, _payload: &Value) -> Result<Value, String> {
            self.0
                .data
                .lock()
                .unwrap()
                .events
                .push("action-start".to_owned());
            self.0.action_entered.notify_one();
            if self.0.hold_action.load(Ordering::SeqCst) {
                self.0.action_release.notified().await;
            }
            self.0
                .data
                .lock()
                .unwrap()
                .events
                .push("action-end".to_owned());
            Ok(json!({"ok":true}))
        }
        async fn connect_session(&mut self, _id: &str) -> Result<(), String> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("connect".to_owned());
            data.authenticated = true;
            Ok(())
        }
        async fn refresh(&mut self, _id: &str) -> Result<Value, String> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("refresh".to_owned());
            if data.refresh_after_claim_fails && data.events.iter().any(|e| e == "claim") {
                return Err("模拟领取后刷新失败".to_owned());
            }
            Ok(data.activity.clone())
        }
        async fn status(&mut self) -> Result<Value, String> {
            let data = self.0.data.lock().unwrap();
            let mut status = data.acceleration.clone();
            status["active"] = json!(data.active);
            Ok(status)
        }
        async fn refresh_membership(&mut self) -> Result<Value, String> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("membership".to_owned());
            Ok(data.membership.clone())
        }
        async fn free_catalog(&mut self) -> Result<HashSet<i64>, String> {
            Ok(self.0.data.lock().unwrap().catalog.clone())
        }
        async fn free_access(
            &mut self,
            id: i64,
            _platform: i64,
            in_free_catalog: bool,
        ) -> Result<FreeAccess, String> {
            let data = self.0.data.lock().unwrap();
            Ok(data
                .free_access
                .get(&id)
                .copied()
                .unwrap_or(if in_free_catalog {
                    FreeAccess::Catalog
                } else {
                    FreeAccess::Unconfirmed
                }))
        }
        async fn start_game(
            &mut self,
            id: i64,
            area: Option<i64>,
            platform: i64,
        ) -> Result<Value, String> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("start".to_owned());
            data.starts.push((id, area, platform));
            data.active = true;
            data.acceleration =
                json!({"active":true,"gameId":id,"areaId":area,"platformId":platform});
            Ok(data.acceleration.clone())
        }
        async fn stop_game(&mut self, _id: i64) -> Result<(), String> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("stop".to_owned());
            if data.stop_failures > 0 {
                data.stop_failures -= 1;
                return Err("模拟停止失败".to_owned());
            }
            data.active = false;
            Ok(())
        }
        async fn claim(&mut self, id: &str, task: &Value) -> Result<Value, ApiError> {
            {
                let mut data = self.0.data.lock().unwrap();
                data.events.push("claim".to_owned());
                data.claims.push((id.to_owned(), task.clone()));
            }
            self.0.claim_entered.notify_one();
            if self.0.hold_claim.load(Ordering::SeqCst) {
                self.0.claim_release.notified().await;
            }
            let mut data = self.0.data.lock().unwrap();
            if data
                .claim_error_task
                .as_ref()
                .is_none_or(|target| target == task)
            {
                if let Some(error) = data.claim_error.clone() {
                    return Err(error);
                }
            }
            assert_eq!(data.activity["id"], id);
            let current = data.activity["tasks"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|current| current["id"] == *task)
                .expect("模拟领取必须使用原始任务编号");
            current["status"] = json!(3);
            data.activity["tickets"] = json!(data.activity["tickets"].as_u64().unwrap() + 1);
            Ok(json!({"ok":true}))
        }
        async fn draw(&mut self, _id: &str) -> Result<Value, ApiError> {
            let mut data = self.0.data.lock().unwrap();
            data.events.push("draw".to_owned());
            if let Some(error) = data.draw_error.clone() {
                return Err(error);
            }
            if !data.unchanged_tickets {
                data.activity["tickets"] = json!(0);
                if let Some(end) = data.renew_on_draw.take() {
                    data.membership["expiresAt"] = json!(end);
                }
            }
            // 不返回 inventoryDetailId，测试不需要等待奖品详情延迟。
            Ok(json!({"rightsName":"测试奖品"}))
        }
        async fn prize(&mut self, _id: &str, _inventory: &Value) -> Result<Value, String> {
            panic!("本夹具不应请求奖品详情")
        }
    }

    async fn fixture(mock: &Arc<Mock>, storage: Storage) -> Arc<Engine> {
        let engine = Engine::new(Box::new(MockService(mock.clone())), storage, 0)
            .await
            .unwrap();
        engine
            .change(|r| r["day"] = json!(day_key(now_ms())))
            .await
            .unwrap();
        engine
    }
    async fn execute(engine: &Engine) {
        tokio::time::timeout(
            Duration::from_secs(3),
            engine.run_worker(vec!["a".to_owned()]),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn expired_membership_prioritizes_free_games_and_resumes_only_after_verified_renewal() {
        for renewal in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mock = Mock::new(1, 0);
            let now = now_ms();
            {
                let mut data = mock.data.lock().unwrap();
                data.membership = json!({"expiresAt":now - 10_000});
                data.renew_on_draw = renewal.then_some(now + 86_400_000);
                data.activity["tasks"] = json!([
                    {"id":1,"type":7,"status":1,"title":"会员任务","supported":true,"seconds":1},
                    {"id":2,"type":7,"status":1,"title":"免费目录任务","supported":true,"seconds":1},
                    {"id":3,"type":7,"status":1,"title":"限免任务","supported":true,"seconds":1},
                    {"id":4,"type":7,"status":1,"title":"限免已过期任务","supported":true,"seconds":1}
                ]);
                data.mappings = json!({
                    "a:1":{"gameId":41,"gameName":"会员游戏"},
                    "a:2":{"gameId":42,"gameName":"目录游戏"},
                    "a:3":{"gameId":43,"gameName":"限免游戏"},
                    "a:4":{"gameId":44,"gameName":"限免已过期游戏"}
                });
                data.catalog.extend([42, 44]);
                data.free_access.insert(
                    43,
                    FreeAccess::Limited {
                        start: now - 10_000,
                        end: now + 60_000,
                    },
                );
                data.free_access.insert(
                    44,
                    FreeAccess::Limited {
                        start: now - 10_000,
                        end: now - 1,
                    },
                );
            }
            let mut engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
            let test_engine = Arc::get_mut(&mut engine).unwrap();
            test_engine.tick = Duration::from_millis(10);
            test_engine.buffer = 0.0;
            tokio::time::timeout(Duration::from_secs(10), engine.run_worker(vec!["a".into()]))
                .await
                .unwrap();
            let snapshot = engine.snapshot().await;
            let run = &snapshot["run"];
            assert_eq!(run["status"], "completed", "{run}");
            let data = mock.data.lock().unwrap();
            let started: Vec<_> = data.starts.iter().map(|(id, _, _)| *id).collect();
            if renewal {
                assert_eq!(started, vec![43, 41, 42, 44]);
                assert_eq!(snapshot["membership"]["expiresAt"], now + 86_400_000);
                assert!(run["logs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["message"]
                        .as_str()
                        .unwrap()
                        .contains("新的有效会员时长")));
            } else {
                assert_eq!(started, vec![43, 42]);
                assert!(run["message"].as_str().unwrap().contains("2 组"));
            }
            let lottery = data
                .events
                .iter()
                .position(|event| event == "draw")
                .unwrap();
            let membership = data
                .events
                .iter()
                .rposition(|event| event == "membership")
                .unwrap();
            assert!(lottery < membership);
        }
    }

    #[tokio::test]
    async fn available_lottery_can_renew_membership_before_any_game_starts() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(1, 1);
        {
            let mut data = mock.data.lock().unwrap();
            data.membership = json!({"expiresAt":null});
            data.renew_on_draw = Some(now_ms() + 86_400_000);
        }
        let mut engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        let test_engine = Arc::get_mut(&mut engine).unwrap();
        test_engine.tick = Duration::from_millis(10);
        test_engine.buffer = 0.0;
        execute(&engine).await;
        let events = mock.events();
        let first = |event| events.iter().position(|item| item == event).unwrap();
        assert!(first("draw") < first("membership"));
        assert!(first("membership") < first("start"));
        assert_eq!(engine.snapshot().await["run"]["status"], "completed");
    }

    fn warframe_mock(pc_status: i64, mobile_status: i64) -> Arc<Mock> {
        let task = |id: Value, title: &str, status: i64| {
            let mut task = json!({
                "id":id,"type":7,"status":status,"title":title,"finishCount":0
            });
            let requirement = crate::domain::task_requirement(&task);
            task.as_object_mut()
                .unwrap()
                .extend(requirement.as_object().unwrap().clone());
            task
        };
        let mock = Mock::new(pc_status, 0);
        {
            let mut data = mock.data.lock().unwrap();
            // 保留两端原始编号，PC 领取不能改变移动奖励记录。
            data.activity["tasks"] = json!([
                task(json!(101), "加速《Warframe》PC端达到1秒", pc_status),
                task(
                    json!("mobile-202"),
                    "加速《星际战甲》移动端达到2秒",
                    mobile_status
                )
            ]);
            data.mappings = json!({
                "a:101":{"gameId":42,"areaId":9,"gameName":"Warframe","platformId":6},
                "a:mobile-202":{"gameId":42,"areaId":9,"gameName":"Warframe","platformId":6,"manual":true,"seconds":2}
            });
        }
        mock
    }

    #[tokio::test]
    async fn warframe_pc_accelerates_and_claims_without_timing_the_legacy_mobile_task() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let mock = warframe_mock(1, 1);
        {
            let mut data = mock.data.lock().unwrap();
            data.activity["tasks"][1]["supported"] = json!(true);
            data.activity["tasks"][1]["sharedAcceleration"] = json!("warframe-pc");
            data.activity["tasks"][1]["seconds"] = json!(2);
        }
        let mut engine = fixture(&mock, storage.clone()).await;
        // accelerate 用局部 elapsed 和 std::time::Instant，run.elapsed 不能快进。
        // 仅缩短测试实例的采样间隔/缓冲，保留 PC 的真实 1 秒合法阈值。
        let test_engine = Arc::get_mut(&mut engine).unwrap();
        test_engine.tick = Duration::from_millis(5);
        test_engine.buffer = 0.0;
        let plan = engine.snapshot().await["plan"].clone();
        let groups = plan["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["platformId"], 6);
        assert_eq!(groups[0]["seconds"], 1);
        assert_eq!(groups[0]["tasks"][0]["taskId"], 101);
        assert_eq!(groups[0]["tasks"][0]["seconds"], 1);
        assert_eq!(groups[0]["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(plan["skipped"][0]["key"], "a:mobile-202");
        assert_eq!(plan["totalSeconds"], 1);

        tokio::time::timeout(
            Duration::from_secs(10),
            engine.run_worker(vec!["a".to_owned()]),
        )
        .await
        .unwrap();
        let snapshot = engine.snapshot().await;
        assert_eq!(snapshot["run"]["status"], "completed");
        assert!(snapshot["run"]["elapsed"].as_f64().unwrap() >= 1.0);
        assert!(snapshot["run"]["pending"].is_null());
        assert!(snapshot["run"]["ownedGameId"].is_null());
        assert_eq!(snapshot["run"]["results"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["activities"][0]["tasks"][0]["status"], 3);
        assert_eq!(snapshot["activities"][0]["tasks"][1]["status"], 1);
        assert_eq!(snapshot["activities"][0]["tasks"][1]["finishCount"], 0);
        assert_eq!(snapshot["activities"][0]["tickets"], 0);
        assert_eq!(
            storage.read("run.json", Value::Null).await.unwrap()["pending"],
            Value::Null
        );
        let data = mock.data.lock().unwrap();
        assert_eq!(data.starts, vec![(42, Some(9), 6)]);
        assert_eq!(data.claims, vec![("a".to_owned(), json!(101))]);
        assert!(!data.active);
        assert_eq!(
            data.events
                .iter()
                .filter(|event| matches!(event.as_str(), "start" | "claim" | "draw" | "stop"))
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["start", "claim", "draw", "stop"]
        );
    }

    #[tokio::test]
    async fn warframe_ready_mobile_reward_is_not_claimed_when_pc_is_already_claimed() {
        let root = tempfile::tempdir().unwrap();
        let mock = warframe_mock(3, 2);
        {
            let mut data = mock.data.lock().unwrap();
            data.activity["tasks"][1]["supported"] = json!(true);
            data.activity["tasks"][1]["sharedAcceleration"] = json!("warframe-pc");
        }
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        execute(&engine).await;
        let snapshot = engine.snapshot().await;
        assert_eq!(snapshot["run"]["status"], "error");
        assert_eq!(
            snapshot["run"]["message"],
            "所选活动没有可运行任务或可用抽奖次数"
        );
        assert!(snapshot["run"]["pending"].is_null());
        assert_eq!(snapshot["run"]["results"], json!([]));
        assert_eq!(snapshot["plan"]["groups"], json!([]));
        assert_eq!(snapshot["activities"][0]["tasks"][0]["status"], 3);
        assert_eq!(snapshot["activities"][0]["tasks"][1]["status"], 2);
        let data = mock.data.lock().unwrap();
        assert!(data.starts.is_empty());
        assert!(data.claims.is_empty());
        assert!(!data.events.iter().any(|event| event == "draw"));
        assert!(!data.events.iter().any(|event| event == "stop"));
    }

    #[tokio::test]
    async fn warframe_ready_pc_reward_is_claimed_while_ready_mobile_reward_is_untouched() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let mock = warframe_mock(2, 2);
        {
            let mut data = mock.data.lock().unwrap();
            data.activity["tasks"][1]["supported"] = json!(true);
            data.activity["tasks"][1]["sharedAcceleration"] = json!("warframe-pc");
        }
        let engine = fixture(&mock, storage.clone()).await;
        execute(&engine).await;
        let snapshot = engine.snapshot().await;
        assert_eq!(snapshot["run"]["status"], "completed");
        assert!(snapshot["run"]["pending"].is_null());
        assert_eq!(snapshot["activities"][0]["tasks"][0]["status"], 3);
        assert_eq!(snapshot["activities"][0]["tasks"][1]["status"], 2);
        assert_eq!(snapshot["run"]["results"].as_array().unwrap().len(), 1);
        assert_eq!(
            storage.read("run.json", Value::Null).await.unwrap()["pending"],
            Value::Null
        );
        let data = mock.data.lock().unwrap();
        assert_eq!(data.claims, vec![("a".to_owned(), json!(101))]);
        assert!(data.starts.is_empty());
        assert_eq!(
            data.events.iter().filter(|event| *event == "draw").count(),
            1
        );
    }

    #[tokio::test]
    async fn ready_tasks_ignore_legacy_sharing_marker_for_every_game_name() {
        for name in ["Warframe", "其他游戏"] {
            for marked in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let mock = Mock::new(2, 0);
                {
                    let mut data = mock.data.lock().unwrap();
                    data.activity["tasks"][0]["title"] = json!(format!("加速《{name}》PC端10分钟"));
                    if marked {
                        data.activity["tasks"][0]["sharedAcceleration"] = json!("warframe-pc");
                    }
                }
                let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
                assert_eq!(engine.snapshot().await["plan"]["totalSeconds"], 0);
                execute(&engine).await;
                let snapshot = engine.snapshot().await;
                assert_eq!(snapshot["run"]["status"], "completed");
                assert!(snapshot["run"]["pending"].is_null());
                assert_eq!(snapshot["activities"][0]["tasks"][0]["status"], 3);
                assert_eq!(snapshot["run"]["results"].as_array().unwrap().len(), 1);
                let data = mock.data.lock().unwrap();
                assert!(data.starts.is_empty());
                assert_eq!(data.claims, vec![("a".to_owned(), json!(1))]);
            }
        }
    }

    #[tokio::test]
    async fn stale_groups_and_direct_collection_reject_mobile_tasks_in_every_state() {
        for status in [1, 2, 3] {
            for cached_reason_only in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let mock = warframe_mock(3, status);
                {
                    let mut data = mock.data.lock().unwrap();
                    data.activity["tasks"][1]["supported"] = json!(true);
                    if cached_reason_only {
                        data.activity["tasks"][1]["title"] = json!("旧奖励");
                    }
                }
                let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
                // 旧计划没有说明，执行时必须以最新任务要求再次排除。
                let task = json!({"activityId":"a","taskId":"mobile-202","title":"旧队列任务",
                    "seconds":2,"status":status,"manual":true});
                let group =
                    json!({"gameId":42,"gameName":"Warframe","platformId":6,"tasks":[task]});
                assert!(engine.prepare_group(&group).await.unwrap().is_empty());
                assert_eq!(
                    engine.collect(&task).await.unwrap_err(),
                    "这是手机端加速任务，PC 客户端不计入"
                );
                let snapshot = engine.snapshot().await;
                assert_eq!(snapshot["run"]["elapsed"], 0);
                assert!(snapshot["run"]["pending"].is_null());
                assert!(snapshot["run"]["ownedGameId"].is_null());
                assert_eq!(snapshot["activities"][0]["tasks"][1]["status"], status);
                let data = mock.data.lock().unwrap();
                assert!(data.starts.is_empty());
                assert!(data.claims.is_empty());
                assert!(!data.events.iter().any(|event| event == "draw"));
            }
        }
    }

    #[tokio::test]
    async fn pending_survives_legacy_cache_loading_and_blocks_resubmission() {
        for kind in ["claim", "claim-reconcile"] {
            let root = tempfile::tempdir().unwrap();
            let storage = Storage::new(root.path().to_owned()).unwrap();
            let pending =
                json!({"kind":kind,"activityId":"a","taskId":"mobile-202","startedAt":123});
            let saved = json!({"status":"running","day":day_key(now_ms()),"pending":pending,
                "ownedGameId":42,"logs":[{"message":"移动奖励结果待核对"}],
                "results":[{"prizeName":"旧 PC 奖品"}]});
            storage.write("run.json", &saved).await.unwrap();
            storage
                .write(
                    "activities.json",
                    &json!({"entries":[{"activeLink":crate::domain::public_url("a")}],
                    "activities":[{"id":"a","tickets":0,"tasks":[
                        {"id":"mobile-202","type":7,"status":2,"finishCount":4,"title":"旧奖励",
                            "supported":true,"seconds":2,"sharedAcceleration":"warframe-pc"}
                    ]}]}),
                )
                .await
                .unwrap();
            storage
                .write(
                    "mappings.json",
                    &json!({"a:mobile-202":{"gameId":42,"manual":true,"seconds":2}}),
                )
                .await
                .unwrap();
            let original_run_bytes = std::fs::read(root.path().join("run.json")).unwrap();
            let backend = Backend::new(storage.clone(), 9222, root.path().join("unused.exe"))
                .await
                .unwrap();
            let engine = Engine::new(Box::new(backend), storage.clone(), 0)
                .await
                .unwrap();
            let snapshot = engine.snapshot().await;
            assert_eq!(snapshot["activities"][0]["tasks"][0]["supported"], true);
            assert_eq!(
                snapshot["activities"][0]["tasks"][0]["sharedAcceleration"],
                "warframe-pc"
            );
            assert_eq!(snapshot["activities"][0]["tasks"][0]["id"], "mobile-202");
            assert_eq!(snapshot["activities"][0]["tasks"][0]["status"], 2);
            assert_eq!(snapshot["activities"][0]["tasks"][0]["finishCount"], 4);
            assert_eq!(snapshot["plan"]["groups"].as_array().unwrap().len(), 1);
            assert_eq!(snapshot["plan"]["totalSeconds"], 0);
            assert_eq!(snapshot["mappings"]["a:mobile-202"]["gameId"], 42);
            assert_eq!(snapshot["run"]["status"], "interrupted");
            assert_eq!(snapshot["run"]["pending"], pending);
            assert_eq!(snapshot["run"]["ownedGameId"], 42);
            assert_eq!(snapshot["run"]["logs"], saved["logs"]);
            assert_eq!(snapshot["run"]["results"], saved["results"]);
            assert!(engine
                .dispatch("start", json!({"ids":["a"]}))
                .await
                .unwrap_err()
                .contains("待核对"));
            assert_eq!(engine.snapshot().await["run"]["pending"], pending);
            assert_eq!(
                std::fs::read(root.path().join("run.json")).unwrap(),
                original_run_bytes
            );
        }
    }

    #[tokio::test]
    async fn repeated_connection_failures_are_visible_and_persisted_with_startup_phases() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let backend = Backend::new(storage.clone(), port, root.path().join("biubiu.exe"))
            .await
            .unwrap();
        let engine = Engine::new(Box::new(backend), storage.clone(), port)
            .await
            .unwrap();
        for _ in 0..2 {
            let error = engine.dispatch("connect", json!({})).await.unwrap_err();
            assert!(error.contains("请选择实际存在"), "{error}");
        }
        let snapshot = engine.snapshot().await;
        let logs = snapshot["run"]["logs"].as_array().unwrap();
        assert_eq!(
            logs.iter()
                .filter(|log| log["message"] == "开始连接 biubiu 客户端")
                .count(),
            2
        );
        assert_eq!(logs.iter().filter(|log| log["level"] == "error").count(), 2);
        assert_eq!(
            storage.read("run.json", Value::Null).await.unwrap()["logs"],
            snapshot["run"]["logs"]
        );
        let events = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        let events: Vec<Value> = events
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["phase"] == "launch")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event["level"] == "error")
                .count(),
            2
        );
        assert!(!engine.has_operation());
    }

    #[tokio::test]
    async fn settings_save_updates_snapshot_storage_and_the_task_lease_port() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("biubiu.exe");
        std::fs::write(&exe, b"fixture").unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        storage
            .write(
                "activities.json",
                &json!({"entries":[],"activities":[{"id":"a","tasks":[]}]}),
            )
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let old_port = if port == 9222 { 9223 } else { 9222 };
        let backend = Backend::new(storage.clone(), old_port, exe.clone())
            .await
            .unwrap();
        let engine = Engine::new(Box::new(backend), storage.clone(), old_port)
            .await
            .unwrap();
        let payload = json!({"executablePath":exe,"debugPort":port});
        engine
            .dispatch("client-settings", payload.clone())
            .await
            .unwrap();
        assert_eq!(engine.port.load(Ordering::SeqCst), port);
        assert_eq!(engine.snapshot().await["clientSettings"], payload);
        assert_eq!(
            storage.read("settings.json", Value::Null).await.unwrap(),
            payload
        );
        let _lease = Lease::acquire(port).unwrap();
        let error = engine
            .dispatch("start", json!({"ids":["a"]}))
            .await
            .unwrap_err();
        assert!(error.contains("另一份 AutoBiubiu"), "{error}");
        assert!(!engine.busy.load(Ordering::SeqCst));
        // 被租约拦截，未运行队列、连接端口或启动模拟可执行文件。
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn settings_cannot_change_during_running_pending_owned_or_another_operation() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(2, 0);
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        let payload = json!({"executablePath":"missing.exe","debugPort":9333});
        engine.busy.store(true, Ordering::SeqCst);
        assert!(engine
            .dispatch("client-settings", payload.clone())
            .await
            .unwrap_err()
            .contains("请先停止任务"));
        engine.busy.store(false, Ordering::SeqCst);
        engine
            .change(|r| r["pending"] = json!({"kind":"claim"}))
            .await
            .unwrap();
        assert!(engine
            .dispatch("client-settings", payload.clone())
            .await
            .unwrap_err()
            .contains("请先核对"));
        engine
            .change(|r| {
                r["pending"] = Value::Null;
                r["ownedGameId"] = json!(42);
            })
            .await
            .unwrap();
        assert!(engine
            .dispatch("client-settings", payload.clone())
            .await
            .unwrap_err()
            .contains("请先核对"));
        engine
            .change(|r| r["ownedGameId"] = Value::Null)
            .await
            .unwrap();
        let _gate = engine.gate.lock().await;
        assert!(engine
            .dispatch("client-settings", payload)
            .await
            .unwrap_err()
            .contains("正在处理请求"));
        assert!(!root.path().join("settings.json").exists());
        assert_eq!(engine.port.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn queue_claims_ready_task_draws_and_publishes_connection() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(2, 0);
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        execute(&engine).await;
        let snapshot = engine.snapshot().await;
        assert_eq!(snapshot["authenticated"], true);
        assert_eq!(snapshot["run"]["status"], "completed");
        assert!(snapshot["run"]["pending"].is_null());
        assert_eq!(snapshot["run"]["results"][0]["prizeName"], "测试奖品");
        assert_eq!(mock.events().iter().filter(|e| *e == "claim").count(), 1);
        assert_eq!(mock.events().iter().filter(|e| *e == "draw").count(), 1);
        assert!(!mock.events().iter().any(|e| e == "start"));
    }

    #[tokio::test]
    async fn claim_timeout_preserves_pending_on_disk_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let mock = Mock::new(2, 0);
        mock.data.lock().unwrap().claim_error = Some(ApiError {
            message: "模拟领取超时".to_owned(),
            definitive: false,
        });
        let engine = fixture(&mock, storage.clone()).await;
        execute(&engine).await;
        assert_eq!(engine.snapshot().await["run"]["pending"]["kind"], "claim");
        assert_eq!(
            storage.read("run.json", Value::Null).await.unwrap()["pending"]["kind"],
            "claim"
        );
        let restored = Engine::new(Box::new(MockService(mock.clone())), storage, 0)
            .await
            .unwrap();
        assert_eq!(restored.snapshot().await["run"]["pending"]["kind"], "claim");
        assert!(restored
            .dispatch("start", json!({"ids":["a"]}))
            .await
            .is_err());
        assert!(!mock.events().iter().any(|e| e == "draw"));
    }

    #[tokio::test]
    async fn successful_claim_with_failed_refresh_is_still_pending() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(2, 0);
        mock.data.lock().unwrap().refresh_after_claim_fails = true;
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        execute(&engine).await;
        assert_eq!(engine.snapshot().await["run"]["pending"]["kind"], "claim");
        assert!(!mock.events().iter().any(|e| e == "draw"));
    }

    #[tokio::test]
    async fn unchanged_tickets_keeps_draw_result_and_blocks_repeat() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(3, 1);
        mock.data.lock().unwrap().unchanged_tickets = true;
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let engine = fixture(&mock, storage.clone()).await;
        execute(&engine).await;
        let saved = storage.read("run.json", Value::Null).await.unwrap();
        assert_eq!(saved["pending"]["kind"], "draw-reconcile");
        assert_eq!(saved["results"].as_array().unwrap().len(), 1);
        assert_eq!(saved["results"][0]["result"]["rightsName"], "测试奖品");
        assert!(engine
            .dispatch("start", json!({"ids":["a"]}))
            .await
            .is_err());
        assert_eq!(mock.events().iter().filter(|e| *e == "draw").count(), 1);
    }

    #[tokio::test]
    async fn stopping_waits_for_claim_but_never_starts_a_draw() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(2, 0);
        mock.hold_claim.store(true, Ordering::SeqCst);
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        engine.busy.store(true, Ordering::SeqCst);
        let task = {
            let engine = engine.clone();
            tokio::spawn(async move {
                execute(&engine).await;
            })
        };
        tokio::time::timeout(Duration::from_secs(3), mock.claim_entered.notified())
            .await
            .unwrap();
        engine.stop().await.unwrap();
        assert!(!task.is_finished());
        mock.claim_release.notify_one();
        task.await.unwrap();
        assert_eq!(engine.snapshot().await["run"]["status"], "stopped");
        assert!(!mock.events().iter().any(|e| e == "draw"));
    }

    #[tokio::test]
    async fn stop_failure_preserves_owned_id_for_restart_and_retry() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let mock = Mock::new(3, 0);
        mock.data.lock().unwrap().stop_failures = 1;
        let engine = fixture(&mock, storage.clone()).await;
        engine
            .change(|r| r["ownedGameId"] = json!(42))
            .await
            .unwrap();
        assert!(engine.dispatch("stop-owned", json!({})).await.is_err());
        let saved = storage.read("run.json", Value::Null).await.unwrap();
        assert_eq!(saved["ownedGameId"], 42);
        assert_eq!(saved["cleanupError"], "模拟停止失败");
        let restored = Engine::new(Box::new(MockService(mock.clone())), storage.clone(), 0)
            .await
            .unwrap();
        restored.dispatch("stop-owned", json!({})).await.unwrap();
        let saved = storage.read("run.json", Value::Null).await.unwrap();
        assert!(saved["ownedGameId"].is_null());
        assert!(saved["cleanupError"].is_null());
        assert!(restored.dispatch("stop-owned", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn shutdown_waits_for_existing_action_and_rejects_new_actions() {
        let root = tempfile::tempdir().unwrap();
        let mock = Mock::new(3, 0);
        mock.hold_action.store(true, Ordering::SeqCst);
        let engine = fixture(&mock, Storage::new(root.path().to_owned()).unwrap()).await;
        let action = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.dispatch("connect", json!({})).await })
        };
        tokio::time::timeout(Duration::from_secs(3), mock.action_entered.notified())
            .await
            .unwrap();
        let closing = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.shutdown().await })
        };
        while !engine.closing.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert!(!closing.is_finished());
        assert!(engine.dispatch("connect", json!({})).await.is_err());
        mock.action_release.notify_one();
        action.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(mock.events(), vec!["action-start", "action-end"]);
    }
}
