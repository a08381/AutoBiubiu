use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
};

/// 与旧 Node 版共用 JSON 锁；额外文件锁由操作系统在进程退出时释放。
pub struct Lease {
    path: PathBuf,
    token: String,
    _guard: File,
}

impl Lease {
    pub fn acquire(port: u16) -> Result<Self, String> {
        Self::at(
            std::env::temp_dir()
                .join("autobiubiu")
                .join(format!("automation-{port}.lock")),
        )
    }

    fn at(path: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(path.parent().ok_or("运行锁路径无效")?).map_err(|e| e.to_string())?;
        let guard = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("guard"))
            .map_err(|e| e.to_string())?;
        guard
            .try_lock_exclusive()
            .map_err(|_| "另一份 AutoBiubiu 正在执行任务".to_owned())?;
        let token = format!("{}-{}", std::process::id(), crate::domain::now_ms());
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let written = file
                        .write_all(
                            json!({"pid":std::process::id(),"token":token})
                                .to_string()
                                .as_bytes(),
                        )
                        .and_then(|_| file.sync_all());
                    if let Err(error) = written {
                        let _ = fs::remove_file(&path);
                        return Err(error.to_string());
                    }
                    return Ok(Self {
                        path,
                        token,
                        _guard: guard,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let owner: Value =
                        serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                            .map_err(|_| "旧版运行锁异常，请检查此前任务是否已结束".to_owned())?;
                    let pid = owner["pid"]
                        .as_u64()
                        .filter(|p| *p > 0 && *p <= u32::MAX as u64)
                        .ok_or("旧版运行锁的进程编号无效")? as u32;
                    if process_alive(pid) {
                        return Err("另一份 AutoBiubiu 正在执行任务，请先退出旧版".to_owned());
                    }
                    fs::remove_file(&path).map_err(|e| e.to_string())?;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("获取运行锁失败".to_owned())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(bytes) = fs::read(&self.path) {
            if let Ok(owner) = serde_json::from_slice::<Value>(&bytes) {
                if owner["token"].as_str() == Some(&self.token) {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // 查询失败但不是“进程不存在”时，保守地视为仍在运行。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok == 0 || code == 259
    }
}

#[cfg(not(windows))]
pub fn process_alive(pid: u32) -> bool {
    // `kill -0` 的非零退出码同时表示 ESRCH 与 EPERM；后者代表进程仍在，
    // 不能像简单的 status.success() 那样把权限不足误判为空闲锁。
    match std::process::Command::new("kill")
        .env("LC_ALL", "C")
        .args(["-0", &pid.to_string()])
        .output()
    {
        Ok(output) if output.status.success() => true,
        Ok(output) => !String::from_utf8_lossy(&output.stderr)
            .to_ascii_lowercase()
            .contains("no such process"),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.lock");
        let first = Lease::at(path.clone()).unwrap();
        assert!(Lease::at(path.clone()).is_err());
        drop(first);
        assert!(!path.exists());
        assert!(Lease::at(path).is_ok());
    }
    #[test]
    fn respects_legacy_live_owner_and_corrupt_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.lock");
        fs::write(&path, json!({"pid":std::process::id()}).to_string()).unwrap();
        assert!(Lease::at(path.clone()).is_err());
        fs::write(&path, "{broken").unwrap();
        assert!(Lease::at(path).is_err());
    }
}
