use crate::{client_platform, storage::Storage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientSettings {
    pub executable_path: String,
    pub debug_port: u16,
}

impl ClientSettings {
    pub async fn load(storage: &Storage, initial_port: u16, detect: bool) -> Result<Self, String> {
        let saved = storage.read("settings.json", Value::Null).await?;
        if !saved.is_null() {
            let settings: Self = serde_json::from_value(saved)
                .map_err(|_| "客户端设置格式无效，请检查 data/settings.json".to_owned())?;
            if settings.debug_port == 0 {
                return Err("保存的调试端口必须是 1–65535".into());
            }
            // 安装位置失效时仍允许打开设置页面修正，不自动覆盖用户配置。
            return Ok(settings);
        }
        let installations = if detect {
            tokio::task::spawn_blocking(client_platform::discover_installations)
                .await
                .map_err(|e| format!("自动识别安装目录失败：{e}"))??
        } else {
            Vec::new()
        };
        let settings = Self {
            executable_path: installations
                .first()
                .map(|item| item.executable_path.clone())
                .unwrap_or_default(),
            debug_port: initial_port,
        };
        if settings.debug_port == 0 {
            return Err("调试端口必须是 1–65535".into());
        }
        storage
            .write(
                "settings.json",
                &serde_json::to_value(&settings).map_err(|e| e.to_string())?,
            )
            .await?;
        Ok(settings)
    }

    pub fn from_input(payload: &Value) -> Result<Self, String> {
        let mut settings: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "请填写安装位置，以及 1–65535 之间的整数端口".to_owned())?;
        if settings.debug_port == 0 {
            return Err("调试端口必须是 1–65535".into());
        }
        let path = PathBuf::from(settings.executable_path.trim().trim_matches('"'));
        let path = if path.is_dir() {
            path.join("biubiu.exe")
        } else {
            path
        };
        let path = client_platform::validate_executable(&path)?;
        settings.executable_path = path.to_string_lossy().into_owned();
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn accepts_directory_or_executable_and_rejects_bad_port() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("biubiu.exe");
        std::fs::write(&exe, b"fixture").unwrap();
        let settings =
            ClientSettings::from_input(&json!({"executablePath":root.path(),"debugPort":9333}))
                .unwrap();
        assert_eq!(settings.executable_path, exe.to_string_lossy());
        for port in [json!(0), json!(-1), json!(65536), json!(1.5), json!("9222")] {
            assert!(
                ClientSettings::from_input(&json!({"executablePath":exe,"debugPort":port}))
                    .is_err()
            );
        }
        assert!(ClientSettings::from_input(
            &json!({"executablePath":"relative/biubiu.exe","debugPort":9222})
        )
        .is_err());
    }
    #[tokio::test]
    async fn saved_settings_win_over_first_start_defaults_even_if_path_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let saved = json!({"executablePath":"missing/biubiu.exe","debugPort":9444});
        storage.write("settings.json", &saved).await.unwrap();
        let loaded = ClientSettings::load(&storage, 9222, false).await.unwrap();
        assert_eq!(loaded.debug_port, 9444);
        assert_eq!(loaded.executable_path, "missing/biubiu.exe");
        assert_eq!(
            storage.read("settings.json", Value::Null).await.unwrap(),
            saved
        );
    }
    #[tokio::test]
    async fn first_start_without_detection_saves_only_configuration() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_owned()).unwrap();
        let settings = ClientSettings::load(&storage, 9222, false).await.unwrap();
        assert!(settings.executable_path.is_empty());
        assert_eq!(settings.debug_port, 9222);
        assert!(root.path().join("settings.json").is_file());
        assert!(!root.path().join("run.json").exists());
    }
}
