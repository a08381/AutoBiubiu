use fs2::FileExt;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
const MIGRATION_FILES: [&str; 5] = [
    "activities.json",
    "mappings.json",
    "run.json",
    "events.jsonl",
    "settings.json",
];

/// Clones share the write queue. An OS file lock also serializes separate instances.
#[derive(Clone)]
pub struct Storage {
    dir: Arc<PathBuf>,
    queue: Arc<Mutex<()>>,
}

impl Storage {
    pub fn new(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("建立数据目录失败：{e}"))?;
        Ok(Self {
            dir: Arc::new(dir),
            queue: Arc::new(Mutex::new(())),
        })
    }

    pub fn dir(&self) -> &Path {
        self.dir.as_path()
    }

    pub async fn read(&self, name: &str, default: Value) -> Result<Value, String> {
        let path = self.path(name)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("状态文件 {name} 损坏：{e}"))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(default),
            Err(e) => Err(format!("读取状态文件 {name} 失败：{e}")),
        }
    }

    pub async fn write(&self, name: &str, value: &Value) -> Result<(), String> {
        let path = self.path(name)?;
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("状态序列化失败：{e}"))?;
        let dir = self.dir.clone();
        let guard = self.queue.clone().lock_owned().await;
        // Move the guard into the blocking operation: cancelling the caller must not
        // release serialization while its rename is still in progress.
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _lock = directory_lock(&dir)?;
            // A damaged state is evidence, not an empty state to overwrite.
            match fs::read(&path) {
                Ok(old) => {
                    serde_json::from_slice::<Value>(&old)
                        .map_err(|e| format!("状态文件 {} 损坏：{e}", path.display()))?;
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(format!("读取原状态失败：{e}")),
            }
            atomic_write(&path, &bytes)
        })
        .await
        .map_err(|e| format!("状态写入任务失败：{e}"))?
    }

    pub async fn log(&self, event: &Value) -> Result<(), String> {
        let mut redacted = event.clone();
        redact_secrets(&mut redacted);
        let mut bytes =
            serde_json::to_vec(&redacted).map_err(|e| format!("日志序列化失败：{e}"))?;
        bytes.push(b'\n');
        let dir = self.dir.clone();
        let guard = self.queue.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _lock = directory_lock(&dir)?;
            let mut options = OpenOptions::new();
            options.create(true).append(true);
            private_mode(&mut options);
            let mut file = options
                .open(dir.join("events.jsonl"))
                .map_err(|e| format!("打开日志失败：{e}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_data())
                .map_err(|e| format!("写入日志失败：{e}"))
        })
        .await
        .map_err(|e| format!("日志写入任务失败：{e}"))?
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty()
            || name.len() > 128
            || name.starts_with('.')
            || name.ends_with('.')
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err("状态文件名无效".to_owned());
        }
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix)
                    .map(|s| s.len() == 1 && matches!(s.as_bytes()[0], b'1'..=b'9'))
                    .unwrap_or(false)
            })
        {
            return Err("状态文件名无效".to_owned());
        }
        Ok(self.dir.join(name))
    }
}

fn private_mode(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = options;
}

fn directory_lock(dir: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    private_mode(&mut options);
    let file = options
        .open(dir.join(".storage.lock"))
        .map_err(|e| format!("打开存储锁失败：{e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            // Windows 的 ERROR_LOCK_VIOLATION 未必被映射为 WouldBlock，
            // 同时匹配 fs2 提供的平台锁竞争错误，保持独立 Storage 实例串行写入。
            Err(e)
                if (e.kind() == ErrorKind::WouldBlock
                    || e.raw_os_error() == fs2::lock_contended_error().raw_os_error())
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("获取存储锁失败：{e}")),
        }
    }
}

fn unique_suffix() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        crate::domain::now_ms(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    )
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "状态文件目录无效".to_owned())?;
    let temp = parent.join(format!(".state-{}.tmp", unique_suffix()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    private_mode(&mut options);
    let mut file = options
        .open(&temp)
        .map_err(|e| format!("建立临时状态失败：{e}"))?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("写入临时状态失败：{e}"))?;
        drop(file);
        // Rust uses replace-existing rename on Windows; never remove the old file first.
        fs::rename(&temp, path).map_err(|e| format!("替换状态文件失败：{e}"))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("同步状态目录失败：{e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn redact_secrets(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let key = key.to_ascii_lowercase().replace(['-', '_'], "");
                if matches!(
                    key.as_str(),
                    "cookie"
                        | "cookies"
                        | "setcookie"
                        | "authorization"
                        | "serviceticket"
                        | "csrftoken"
                        | "xcsrftoken"
                        | "accesstoken"
                        | "refreshtoken"
                ) {
                    *value = Value::String("[redacted]".to_owned());
                } else {
                    redact_secrets(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

/// Call BEFORE Storage::new. Existing destinations are left entirely untouched.
/// Only legacy user records are copied, under the cross-version run lease.
pub async fn prepare_data_directory(
    data_dir: &Path,
    legacy_dir: Option<&Path>,
    debug_port: u16,
) -> Result<(), String> {
    let data_dir = data_dir.to_owned();
    let legacy_dir = legacy_dir.map(Path::to_owned);
    tokio::task::spawn_blocking(move || {
        prepare_directory(&data_dir, legacy_dir.as_deref(), debug_port)
    })
    .await
    .map_err(|e| format!("数据迁移任务失败：{e}"))?
}

fn prepare_directory(
    data_dir: &Path,
    legacy_dir: Option<&Path>,
    debug_port: u16,
) -> Result<(), String> {
    match fs::metadata(data_dir) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => return Err("数据目录路径已被文件占用".to_owned()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("检查数据目录失败：{e}")),
    }
    let _lease = if legacy_dir.is_some() {
        Some(crate::lease::Lease::acquire(debug_port)?)
    } else {
        None
    };
    if let Some(legacy) = legacy_dir {
        check_legacy_lock(&legacy.join("automation.lock"))?;
    }
    let parent = data_dir
        .parent()
        .ok_or_else(|| "数据目录路径无效".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("建立数据父目录失败：{e}"))?;
    let staging = parent.join(format!(".data-migration-{}", unique_suffix()));
    fs::create_dir(&staging).map_err(|e| format!("建立迁移暂存目录失败：{e}"))?;
    let result = (|| {
        if let Some(legacy) = legacy_dir {
            for name in MIGRATION_FILES {
                let source = legacy.join(name);
                let bytes = match fs::read(&source) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == ErrorKind::NotFound => continue,
                    Err(e) => return Err(format!("读取旧版 {name} 失败：{e}")),
                };
                // Keep malformed files at the source and report them before installing a profile.
                if name.ends_with(".json") {
                    serde_json::from_slice::<Value>(&bytes)
                        .map_err(|e| format!("旧版状态文件 {name} 损坏：{e}"))?;
                }
                let mut options = OpenOptions::new();
                options.create_new(true).write(true);
                private_mode(&mut options);
                let mut destination = options
                    .open(staging.join(name))
                    .map_err(|e| format!("复制旧版 {name} 失败：{e}"))?;
                destination
                    .write_all(&bytes)
                    .and_then(|_| destination.sync_all())
                    .map_err(|e| format!("复制旧版 {name} 失败：{e}"))?;
            }
        }
        // Recheck after acquiring the lease. Never merge into an existing profile.
        if data_dir.exists() {
            return Ok(());
        }
        fs::rename(&staging, data_dir).map_err(|e| format!("安装迁移数据失败：{e}"))?;
        Ok(())
    })();
    // The only directory ever removed is this invocation's newly-created staging directory.
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn check_legacy_lock(path: &Path) -> Result<(), String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("读取旧版运行锁失败：{e}")),
    };
    let owner: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "旧版运行锁异常，请先检查旧版任务是否已结束".to_owned())?;
    let pid = owner["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "旧版运行锁异常".to_owned())?;
    if crate::lease::process_alive(pid) {
        return Err("旧版仍在执行任务，请先停止旧版任务，再打开桌面版".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn missing_returns_default_but_corrupt_state_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().join("data")).unwrap();
        assert_eq!(
            storage
                .read("run.json", json!({"status":"idle"}))
                .await
                .unwrap()["status"],
            "idle"
        );
        fs::write(storage.dir().join("run.json"), b"broken").unwrap();
        assert!(storage.read("run.json", json!({})).await.is_err());
        assert!(storage.write("run.json", &json!({})).await.is_err());
        assert_eq!(fs::read(storage.dir().join("run.json")).unwrap(), b"broken");
    }

    #[tokio::test]
    async fn clones_and_separate_instances_serialize_atomic_writes() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let other = Storage::new(root.path().to_owned()).unwrap();
        let mut writes = Vec::new();
        for n in 0..30 {
            let writer = if n % 2 == 0 {
                storage.clone()
            } else {
                other.clone()
            };
            writes.push(tokio::spawn(async move {
                writer
                    .write("run.json", &json!({"n":n,"padding":"x".repeat(5000)}))
                    .await
            }));
        }
        for write in writes {
            write.await.unwrap().unwrap();
        }
        let saved = storage.read("run.json", Value::Null).await.unwrap();
        assert_eq!(saved["padding"].as_str().unwrap().len(), 5000);
        storage
            .write("run.json", &json!({"final":true}))
            .await
            .unwrap();
        assert_eq!(
            storage.read("run.json", Value::Null).await.unwrap(),
            json!({"final":true})
        );
    }

    #[tokio::test]
    async fn logs_are_complete_lines_and_redact_session_fields() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        for n in 0..3 {
            storage
                .log(&json!({"n":n,"nested":{"Cookie":"secret","serviceTicket":"secret"}}))
                .await
                .unwrap();
        }
        let text = fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(!text.contains("secret"));
        for line in text.lines() {
            assert!(serde_json::from_str::<Value>(line).is_ok());
        }
    }

    #[tokio::test]
    async fn file_names_stay_in_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        for name in [
            "",
            "..",
            "../a",
            "a/b",
            "a\\b",
            "C:stream",
            "run.json:secret",
            "NUL.json",
            ".storage.lock",
            "a.",
        ] {
            assert!(storage.write(name, &json!({})).await.is_err(), "{name}");
            assert!(storage.read(name, Value::Null).await.is_err(), "{name}");
        }
    }

    #[tokio::test]
    async fn migration_copies_only_user_records_and_never_overwrites_profile() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("legacy");
        let target = root.path().join("profile/data");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("run.json"), r#"{"pending":{"kind":"draw"}}"#).unwrap();
        fs::write(legacy.join("events.jsonl"), "{\"message\":\"test\"}\n").unwrap();
        fs::write(legacy.join("research.txt"), "keep").unwrap();
        // Port zero is a private fixture namespace, never the user's real debug port.
        prepare_data_directory(&target, Some(&legacy), 64990)
            .await
            .unwrap();
        assert_eq!(
            fs::read(target.join("run.json")).unwrap(),
            fs::read(legacy.join("run.json")).unwrap()
        );
        assert!(!target.join("research.txt").exists());
        assert!(legacy.join("research.txt").exists());
        fs::write(target.join("run.json"), "{\"new\":true}").unwrap();
        prepare_data_directory(&target, Some(&legacy), 64990)
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(target.join("run.json")).unwrap(),
            "{\"new\":true}"
        );
    }

    #[test]
    fn legacy_live_or_malformed_lock_blocks_copy() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("automation.lock");
        fs::write(&lock, json!({"pid":std::process::id()}).to_string()).unwrap();
        assert!(check_legacy_lock(&lock).is_err());
        fs::write(&lock, b"corrupt").unwrap();
        assert!(check_legacy_lock(&lock).is_err());
    }

    #[tokio::test]
    async fn migration_copies_appdata_settings_into_running_directory() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("appdata");
        let target = root.path().join("portable").join("data");
        fs::create_dir(&legacy).unwrap();
        fs::write(
            legacy.join("settings.json"),
            r#"{"executablePath":"C:/x/biubiu.exe","debugPort":9222}"#,
        )
        .unwrap();
        fs::write(legacy.join("run.json"), r#"{"status":"idle"}"#).unwrap();
        // 每个迁移测试使用不同的夹具端口，避免并行时争用同一把运行锁。
        prepare_data_directory(&target, Some(&legacy), 64995)
            .await
            .unwrap();
        assert!(target.join("settings.json").is_file());
        assert_eq!(
            fs::read_to_string(target.join("settings.json")).unwrap(),
            r#"{"executablePath":"C:/x/biubiu.exe","debugPort":9222}"#
        );
        assert!(target.join("run.json").is_file());
    }
}
