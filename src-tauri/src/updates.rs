use semver::Version;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const LATEST_API: &str = "https://api.github.com/repos/a08381/AutoBiubiu/releases/latest";
pub const LATEST_URL: &str = "https://github.com/a08381/AutoBiubiu/releases/latest";
const CHECK_INTERVAL: Duration = Duration::from_secs(3600);
const MAX_BODY: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseUpdate {
    pub current_version: String,
    pub latest_version: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

#[derive(Default)]
struct CachedCheck {
    checked_at: Option<Instant>,
    result: Option<Result<Option<ReleaseUpdate>, String>>,
}

pub struct UpdateChecker {
    http: reqwest::Client,
    cache: Mutex<CachedCheck>,
}

impl UpdateChecker {
    pub fn new() -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("AutoBiubiu/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "建立更新检查客户端失败".to_owned())?;
        Ok(Self {
            http,
            cache: Mutex::new(CachedCheck::default()),
        })
    }

    pub async fn check(&self) -> (Result<Option<ReleaseUpdate>, String>, bool) {
        // 更新请求独立于任务引擎；缓存也合并窗口重载产生的并发请求。
        let mut cache = self.cache.lock().await;
        if cache
            .checked_at
            .is_some_and(|at| at.elapsed() < CHECK_INTERVAL)
        {
            if let Some(result) = &cache.result {
                return (result.clone(), false);
            }
        }
        let result = self.fetch().await;
        cache.checked_at = Some(Instant::now());
        cache.result = Some(result.clone());
        (result, true)
    }

    async fn fetch(&self) -> Result<Option<ReleaseUpdate>, String> {
        // 不沿用活动服务的 Cookie 或客户端会话，只读取固定仓库的公开 Release。
        let mut response = self
            .http
            .get(LATEST_API)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    return "GitHub 更新请求超时".to_owned();
                }
                "暂时无法连接 GitHub 检查更新".to_owned()
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!(
                "GitHub 更新检查返回 HTTP {}",
                response.status().as_u16()
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BODY as u64)
        {
            return Err("GitHub Release 响应过大".to_owned());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "读取 GitHub Release 失败".to_owned())?
        {
            if body.len() + chunk.len() > MAX_BODY {
                return Err("GitHub Release 响应过大".to_owned());
            }
            body.extend_from_slice(&chunk);
        }
        release_update(&body, env!("CARGO_PKG_VERSION"))
    }
}

fn release_update(body: &[u8], current: &str) -> Result<Option<ReleaseUpdate>, String> {
    let release: Release =
        serde_json::from_slice(body).map_err(|_| "GitHub Release 数据格式无效".to_owned())?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let tag = release.tag_name.trim();
    let latest = Version::parse(
        tag.strip_prefix('v')
            .or_else(|| tag.strip_prefix('V'))
            .unwrap_or(tag),
    )
    .map_err(|_| "GitHub Release 版本号无效".to_owned())?;
    let current = Version::parse(current).map_err(|_| "当前应用版本号无效".to_owned())?;
    if !latest.pre.is_empty() || latest.cmp_precedence(&current) != Ordering::Greater {
        return Ok(None);
    }
    Ok(Some(ReleaseUpdate {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"tag_name":tag,"draft":false,"prerelease":false})).unwrap()
    }

    #[tokio::test]
    #[ignore = "仅显式验证时访问 GitHub 公开 Release，不接入活动客户端"]
    async fn github_latest_release_live() {
        let checker = UpdateChecker::new().unwrap();
        let (result, checked) = checker.check().await;
        let result = result.unwrap();
        assert!(checked);
        println!(
            "GitHub latest checked: current={}, update={result:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(checker.check().await, (Ok(result), false));
    }

    #[test]
    fn only_newer_semantic_versions_offer_an_update() {
        for (tag, current, expected) in [
            ("v0.3.3", "0.3.2", "0.3.3"),
            ("V0.3.10", "0.3.9", "0.3.10"),
            ("0.10.0", "0.9.9", "0.10.0"),
            ("1.0.0", "0.99.99", "1.0.0"),
            ("0.3.2", "0.3.2-rc.1", "0.3.2"),
        ] {
            assert_eq!(
                release_update(&release(tag), current).unwrap(),
                Some(ReleaseUpdate {
                    current_version: current.to_owned(),
                    latest_version: expected.to_owned(),
                })
            );
        }
        for tag in ["0.3.2", "v0.3.1", "0.2.99", "0.3.2+new-build", "0.4.0-rc.1"] {
            assert_eq!(
                release_update(&release(tag), "0.3.2").unwrap(),
                None,
                "{tag}"
            );
        }
    }

    #[test]
    fn drafts_prereleases_and_malformed_responses_do_not_offer_an_update() {
        for field in ["draft", "prerelease"] {
            let mut data = json!({"tag_name":"0.4.0","draft":false,"prerelease":false});
            data[field] = json!(true);
            assert_eq!(
                release_update(&serde_json::to_vec(&data).unwrap(), "0.3.2").unwrap(),
                None
            );
        }
        for tag in [
            "release-next",
            "0.3",
            "0.03.4",
            "https://example.org/",
            "0.4.0<script>",
        ] {
            assert!(release_update(&release(tag), "0.3.2").is_err(), "{tag}");
        }
        for body in [b"not-json".as_slice(), b"{}", b"{\"tag_name\":42}"] {
            assert!(release_update(body, "0.3.2").is_err());
        }
    }
}
