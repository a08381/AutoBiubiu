use serde_json::{json, Value};

// 在客户端侧筛选字段，不把 serviceTicket、手机号或完整账号对象带回桌面后台。
// validTime 与客户端 Date.now() 比较，协议单位明确为毫秒，不猜测秒或剩余天数。
pub const READ_USER_MEMBERSHIP: &str = r#"window.ipc.send({command:'get-user-info'}).then(u => ({
    loggedIn: !!u?.serviceTicket,
    expiresAt: u?.serviceTicket ? (u?.fullMemberInfo?.validTime ?? u?.fullPlatformMemberValidTime ?? null) : null,
    memberStatus: u?.serviceTicket ? (u?.fullMemberInfo?.status ?? null) : null
}))"#;

pub fn unavailable() -> Value {
    json!({"expiresAt":null,"kind":"unknown","checkedAt":null})
}

pub fn normalize(raw: &Value, checked_at: i64) -> Value {
    if raw["loggedIn"] != true {
        return unavailable();
    }
    let expires_at = milliseconds(&raw["expiresAt"]);
    let kind = match raw["memberStatus"].as_i64() {
        Some(1) => "full",
        Some(2) => "trial",
        _ => "unknown",
    };
    json!({"expiresAt":expires_at,"kind":kind,"checkedAt":checked_at})
}

fn milliseconds(value: &Value) -> Option<i64> {
    let value = value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())?;
    // 只接受 2000–9999 年范围内的明确毫秒值；0、秒时间戳和异常哨兵值不展示成日期。
    (946_684_800_000..=253_402_271_999_999)
        .contains(&value)
        .then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_exact_milliseconds_and_membership_kind() {
        let raw = json!({"loggedIn":true,"expiresAt":1794499199123_i64,"memberStatus":1});
        assert_eq!(
            normalize(&raw, 100),
            json!({
                "expiresAt":1794499199123_i64,"kind":"full","checkedAt":100
            })
        );
        let trial = json!({"loggedIn":true,"expiresAt":"1794499199123","memberStatus":2});
        assert_eq!(normalize(&trial, 100)["kind"], "trial");
        assert_eq!(normalize(&trial, 100)["expiresAt"], 1794499199123_i64);
    }

    #[test]
    fn does_not_invent_expiry_from_missing_seconds_or_sentinel_values() {
        for value in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(1794499199),
            json!("not-a-date"),
            json!(true),
            json!(1794499199123.5_f64),
            json!(i64::MAX),
        ] {
            assert_eq!(
                normalize(&json!({"loggedIn":true,"expiresAt":value}), 100)["expiresAt"],
                Value::Null
            );
        }
    }

    #[test]
    fn clears_membership_after_logout_and_never_copies_account_fields() {
        let raw = json!({"loggedIn":false,"expiresAt":1794499199123_i64,"memberStatus":1});
        assert_eq!(normalize(&raw, 100), unavailable());
        let raw = json!({"loggedIn":true,"expiresAt":1794499199123_i64,"serviceTicket":"fixture-only","mobile":"fixture-only"});
        let normalized = normalize(&raw, 100);
        assert!(normalized.get("serviceTicket").is_none());
        assert!(normalized.get("mobile").is_none());
        assert_eq!(normalized["kind"], "unknown");
    }
}
