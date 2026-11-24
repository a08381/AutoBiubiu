use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreeAccess {
    Limited { start: i64, end: i64 },
    Catalog,
    Unconfirmed,
}

impl FreeAccess {
    pub(crate) fn from_game(info: &Value, in_free_catalog: bool) -> Self {
        let window = &info["limitFreeInfo"];
        let start = crate::membership::milliseconds(&window["startTime"]);
        let end = crate::membership::milliseconds(&window["endTime"]);
        match (start, end) {
            (Some(start), Some(end)) if start < end => Self::Limited { start, end },
            // 不用目录命中覆盖已经结束、尚未开始或只有单侧有效边界的限免时段。
            (None, None) if in_free_catalog => Self::Catalog,
            _ => Self::Unconfirmed,
        }
    }

    // 只表示客户端目录提供的排队依据，不代表实际加速使用了哪项权益。
    pub(crate) fn kind(self, now: i64) -> Option<&'static str> {
        match self {
            Self::Limited { start, end } if start < now && now < end => Some("limited-free"),
            Self::Catalog => Some("free-catalog"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distinguishes_catalog_and_explicit_limited_window() {
        let start = 1_794_499_199_123_i64;
        let end = start + 10_000;
        let info = json!({"limitFreeInfo":{"startTime":start.to_string(),"endTime":end}});
        let access = FreeAccess::from_game(&info, true);
        assert_eq!(access.kind(start), None);
        assert_eq!(access.kind(start + 1), Some("limited-free"));
        assert_eq!(access.kind(end), None);
        assert_eq!(
            FreeAccess::from_game(&json!({}), true).kind(end),
            Some("free-catalog")
        );
        assert_eq!(FreeAccess::from_game(&json!({}), false).kind(end), None);
        let incomplete = json!({"limitFreeInfo":{"endTime":end}});
        assert_eq!(FreeAccess::from_game(&incomplete, true).kind(end), None);
    }
}
