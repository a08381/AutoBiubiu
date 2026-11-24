use chrono::{FixedOffset, TimeZone, Utc};
use regex::Regex;
use serde_json::json;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

pub const ORIGIN: &str = "https://www.biubiu001.com";
pub const ROUTE: &str = "/activity/activityTemplate";

const MOBILE_REASON: &str = "这是手机端加速任务，PC 客户端不计入";
const REQUIREMENT_REASON: &str = "请在任务设置中指定游戏和加速分钟数";

pub const PC_PLATFORM: i64 = 6;

pub fn supported_platform(platform: i64) -> bool {
    matches!(platform, 6..=10)
}

pub fn platform_name(platform: i64) -> &'static str {
    match platform {
        6 => "PC",
        7 => "Switch",
        8 => "PS",
        9 => "Xbox",
        10 => "Steam Deck",
        _ => "未知平台",
    }
}

pub fn task_platform(task: &Value) -> Result<i64, String> {
    let text = format!(
        "{} {} {}",
        text_or_empty(task.get("title")),
        text_or_empty(task.get("subTitle")),
        text_or_empty(task.get("description"))
    );
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    let text = QUOTED
        .get_or_init(|| Regex::new(r"《[^》]*》").expect("游戏名正则表达式无效"))
        .replace_all(&text, "");
    static PLATFORMS: OnceLock<Vec<(i64, Regex)>> = OnceLock::new();
    let platforms = PLATFORMS.get_or_init(|| {
        [
            (7, r"(?i)Switch|任天堂"),
            (8, r"(?i)PS[45]?\s*(?:端|主机|平台)|\bPS[45]?\b|PlayStation"),
            (9, r"(?i)Xbox"),
            (10, r"(?i)Steam\s*Deck"),
        ]
        .into_iter()
        .map(|(id, pattern)| (id, Regex::new(pattern).expect("任务平台正则表达式无效")))
        .collect()
    });
    let matched: Vec<i64> = platforms
        .iter()
        .filter(|(_, pattern)| pattern.is_match(&text))
        .map(|(id, _)| *id)
        .collect();
    match matched.as_slice() {
        [platform] => Ok(*platform),
        [] if text.contains("主机") => Err("主机任务的平台不明确，请确认任务要求".to_owned()),
        [] => match task.get("platformId").filter(|value| !value.is_null()) {
            Some(value) => value
                .as_i64()
                .filter(|platform| supported_platform(*platform))
                .ok_or_else(|| "任务平台无效，请刷新活动".to_owned()),
            None => Ok(PC_PLATFORM),
        },
        _ => Err("任务包含多个主机平台，无法合并计时，请确认任务要求".to_owned()),
    }
}

pub fn normalize_task_platforms(activities: &mut [Value]) {
    for task in activities.iter_mut().flat_map(|activity| {
        activity
            .get_mut("tasks")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
    }) {
        if let Some(reason) = mobile_task_reason(task) {
            task["supported"] = json!(false);
            task["reason"] = json!(reason);
            continue;
        }
        if number_is(task.get("type"), 7) {
            match task_platform(task) {
                Ok(platform) => task["platformId"] = json!(platform),
                Err(reason) => {
                    task["supported"] = json!(false);
                    task["reason"] = json!(reason);
                }
            }
        }
    }
}

pub fn activity_id(url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|_| "不支持的活动页面".to_owned())?;
    // URL 解析器可能消去空的 userinfo，原文中的 @ 也需要检查。
    let before_query = url.split(['?', '#']).next().unwrap_or(url);
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("www.biubiu001.com")
        || parsed.port_or_known_default() != Some(443)
        || parsed.path() != ROUTE
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || before_query.contains('@')
    {
        return Err("不支持的活动页面".to_owned());
    }
    let id = parsed
        .query_pairs()
        .find(|(key, _)| key == "activityUuid")
        .map(|(_, value)| value.into_owned())
        .ok_or_else(|| "活动编号无效".to_owned())?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("活动编号无效".to_owned());
    }
    Ok(id)
}

pub fn public_url(id: &str) -> String {
    // 对齐 encodeURIComponent，而不是把空格编码为 + 的表单编码。
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    format!("{ORIGIN}{ROUTE}?activityUuid={encoded}")
}

pub fn parse_initial_state(html: &str) -> Result<Value, String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"window\.__INITIAL_STATE__\s*=\s*([\s\S]*?);\s*</script>")
            .expect("初始数据正则表达式无效")
    });
    let capture = pattern
        .captures(html)
        .ok_or_else(|| "活动页面格式已变化，未找到初始数据".to_owned())?;
    // 只解析 JSON，不执行脚本，也不对脚本表达式做求值。
    serde_json::from_str(&capture[1]).map_err(|error| format!("活动初始数据不是有效 JSON：{error}"))
}

fn task_text(task: &Value) -> String {
    format!(
        "{} {} {}",
        text_or_empty(task.get("title")),
        text_or_empty(task.get("subTitle")),
        text_or_empty(task.get("description"))
    )
}

fn task_game_name(task: &Value) -> Option<String> {
    let title = text_or_empty(task.get("title"));
    let text = task_text(task);
    static QUOTED_NAME: OnceLock<Regex> = OnceLock::new();
    static PLAIN_NAME: OnceLock<Regex> = OnceLock::new();
    static CLEAN_NAME: OnceLock<Regex> = OnceLock::new();
    let quoted_name = QUOTED_NAME
        .get_or_init(|| Regex::new(r"《([^》]+)》").expect("书名号游戏名正则表达式无效"));
    let plain_name = PLAIN_NAME.get_or_init(|| {
        Regex::new(
            r"(?i)加速\s*(.+?)(?:超|超过|满|达到|[0-9]+(?:\.[0-9]+)?\s*(?:分钟|min|小时|秒))",
        )
        .expect("游戏名正则表达式无效")
    });
    let clean_name = CLEAN_NAME.get_or_init(|| {
        Regex::new(
            r"(?i)(?:PC端|电脑端|PS[45]?端|Xbox端|Switch端|Steam\s*Deck端|手机端|移动端|安卓端|iOS端|手游端|游戏|系列游戏|累计|时长)",
        )
        .expect("游戏名清理正则表达式无效")
    });
    quoted_name
        .captures(&title)
        .map(|capture| capture[1].to_owned())
        .or_else(|| {
            plain_name.captures(&title).and_then(|capture| {
                let cleaned = clean_name.replace_all(&capture[1], "");
                let cleaned = cleaned.strip_suffix("系列").unwrap_or(&cleaned);
                let cleaned = cleaned.trim_matches(js_whitespace);
                // 无游戏名的“加速10分钟”可能把数字拆成名称，继续从说明提取。
                (!cleaned.is_empty() && cleaned.parse::<f64>().is_err()).then(|| cleaned.to_owned())
            })
        })
        .or_else(|| {
            quoted_name
                .captures(&text)
                .map(|capture| capture[1].to_owned())
        })
}

pub(crate) fn mobile_task_reason(task: &Value) -> Option<&'static str> {
    // 保留缓存中的手机端原因，避免说明缺失时被手动映射重新启用。
    (is_mobile(&task_text(task))
        || task.get("reason").and_then(Value::as_str) == Some(MOBILE_REASON))
    .then_some(MOBILE_REASON)
}

pub fn task_requirement(task: &Value) -> Value {
    if !number_is(task.get("type"), 7) {
        return json!({"supported": false, "reason": "此任务需要分享、邀请、绑定账号或其他操作"});
    }
    if let Some(reason) = mobile_task_reason(task) {
        return json!({"supported": false, "reason": reason});
    }
    let text = task_text(task);
    let platform = match task_platform(task) {
        Ok(platform) => platform,
        Err(reason) => return json!({"supported": false, "reason": reason}),
    };

    static DURATION: OnceLock<Regex> = OnceLock::new();
    let duration = DURATION.get_or_init(|| {
        Regex::new(r"(?i)(?:累计)?加速[^。\n]*?([+-]?[0-9]+(?:\.[0-9]+)?)\s*(分钟|min(?:ute)?s?|小时|hours?|h|秒钟?|seconds?|s)")
            .expect("任务时长正则表达式无效")
    });

    // 必须从“加速”之后匹配时长，避免将游戏版本、日期和抽奖次数当作时长。
    let mut seconds: Option<f64> = None;
    for capture in duration.captures_iter(&text) {
        let number = capture[1].parse::<f64>().unwrap_or(f64::INFINITY);
        let unit = capture[2].to_lowercase();
        let multiplier = if unit == "小时" || unit.starts_with('h') {
            3600.0
        } else if unit.starts_with('秒') || unit.starts_with('s') {
            1.0
        } else {
            60.0
        };
        let value = number * multiplier;
        seconds = Some(seconds.map_or(value, |previous| previous.max(value)));
    }
    let name = task_game_name(task);
    let valid_seconds = seconds
        .map(|value| value.is_finite() && value > 0.0 && value <= 86400.0)
        .unwrap_or(false);
    let mut requirement = json!({
        "supported": valid_seconds && name.is_some(),
        "gameName": name, "seconds": seconds.map(number_value), "platformId": platform
    });
    if !valid_seconds || name.is_none() {
        requirement["reason"] = json!(REQUIREMENT_REASON);
    }
    requirement
}

pub fn simplify_activity(entry: &Value, info: &Value, config: &Value) -> Result<Value, String> {
    let link = entry
        .get("activeLink")
        .and_then(Value::as_str)
        .ok_or_else(|| "不支持的活动页面".to_owned())?;
    let id = activity_id(link)?;
    let name = truthy_field(entry, "activeName")
        .or_else(|| truthy_field(config, "pageTitle"))
        .cloned()
        .unwrap_or_else(|| Value::String(id.clone()));
    let mut result = json!({
        "id": id, "name": name, "url": public_url(&id),
        "cover": truthy_field(entry, "activeCoverImg")
            .or_else(|| truthy_field(info, "activeCoverImg")).cloned().unwrap_or(json!("")),
        "isOver": truthy(info.get("isOver")),
        "tickets": number_value(truthy_field(info, "lotteryTicket").map(|value| js_number(Some(value))).unwrap_or(0.0)),
        "rules": truthy_field(config, "ruleContent").cloned().unwrap_or(json!([])),
        "freeGiftModule": truthy_field(info, "freeGiftModule").cloned().unwrap_or(Value::Null),
        "tasks": []
    });
    // 缺少字段与显式 null 不同：省略缺失时间，防止缓存错误活动被误判为过期。
    insert_present(
        &mut result,
        "startTime",
        truthy_field(info, "startTime").or_else(|| entry.get("startTime")),
    );
    insert_present(
        &mut result,
        "endTime",
        truthy_field(info, "endTime").or_else(|| entry.get("endTime")),
    );
    let mut tasks = Vec::new();
    if let Some(raw_tasks) = truthy_field(info, "tasks") {
        let raw_tasks = raw_tasks
            .as_array()
            .ok_or_else(|| "活动任务列表格式无效".to_owned())?;
        for task in raw_tasks {
            if !task.is_object() {
                return Err("活动任务数据格式无效".to_owned());
            }
            let task_id = task
                .get("taskId")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!(format!("type-{}", optional_id(task.get("type")))));
            // 保留所有说明文字，避免解析成功后在缓存、规划阶段丢失游戏名或移动端要求。
            let description = if truthy_field(task, "description").is_some() {
                json!(format!(
                    "{} {}",
                    text_or_empty(task.get("subTitle")),
                    text_or_empty(task.get("description"))
                )
                .trim_matches(js_whitespace))
            } else {
                truthy_field(task, "subTitle").cloned().unwrap_or(json!(""))
            };
            let mut simplified = json!({
                "id": task_id,
                "title": truthy_field(task, "title").cloned().unwrap_or(json!("")),
                "description": description,
                "finishCount": truthy_field(task, "finishCount").cloned().unwrap_or(json!(0))
            });
            for key in ["type", "status", "awardNum"] {
                insert_present(&mut simplified, key, task.get(key));
            }
            if let (Some(output), Value::Object(requirement)) =
                (simplified.as_object_mut(), task_requirement(task))
            {
                output.extend(requirement);
            }
            tasks.push(simplified);
        }
    }
    result["tasks"] = Value::Array(tasks);
    Ok(result)
}

pub fn choose_game(name: &str, candidates: &[Value]) -> Option<Value> {
    let target = normalize_name(name);
    let primary: Vec<&Value> = candidates
        .iter()
        .filter(|candidate| {
            let game = &candidate["gameInfo"];
            ["aliasName", "gameName", "originName"]
                .iter()
                .any(|key| normalize_name(&text_or_empty(game.get(*key))) == target)
        })
        .collect();
    if primary.len() == 1 {
        return Some(primary[0].clone());
    }
    let exact: Vec<&Value> = candidates
        .iter()
        .filter(|candidate| {
            text_or_empty(candidate["gameInfo"].get("aliasSearchName"))
                .split(',')
                .any(|alias| normalize_name(alias) == target)
        })
        .collect();
    if exact.len() == 1 {
        return Some(exact[0].clone());
    }
    if candidates.len() == 1 {
        Some(candidates[0].clone())
    } else {
        None
    }
}

pub fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|&c| {
            !js_whitespace(c)
                && !matches!(
                    c,
                    '：' | '《' | '》' | ':' | '(' | ')' | '（' | '）' | '·' | '-'
                )
        })
        .collect()
}

pub fn build_plan(
    activities: &[Value],
    mappings: &Value,
    selected: Option<&[String]>,
    now: i64,
) -> Value {
    let mut groups: Vec<Value> = Vec::new();
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut skipped = Vec::new();
    for activity in activities {
        if let Some(selected) = selected {
            if !activity
                .get("id")
                .and_then(Value::as_str)
                .map(|id| selected.iter().any(|selected_id| selected_id == id))
                .unwrap_or(false)
            {
                continue;
            }
        }
        // JS 对 undefined 的数值比较为 false；缺失日期本身不是排除理由。
        if truthy(activity.get("isOver"))
            || js_number(activity.get("startTime")) > now as f64
            || js_number(activity.get("endTime")) <= now as f64
        {
            let mut item = json!({"reason": "活动未开始或已结束"});
            insert_present(&mut item, "activityId", activity.get("id"));
            skipped.push(item);
            continue;
        }
        for task in activity
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if !number_is(task.get("type"), 7) {
                continue;
            }
            let key = format!(
                "{}:{}",
                optional_id(activity.get("id")),
                optional_id(task.get("id"))
            );
            if let Some(reason) = mobile_task_reason(task) {
                skipped.push(json!({"key": key, "reason": reason}));
                continue;
            }
            if number_is(task.get("status"), 3) {
                skipped.push(json!({"key": key, "reason": "服务器显示已领取"}));
                continue;
            }
            let mapping = mappings.get(&key);
            let field = |name: &str| mapping.and_then(|mapping| mapping.get(name));
            let seconds = field("seconds")
                .filter(|value| !value.is_null())
                .or_else(|| task.get("seconds"))
                .and_then(Value::as_f64);
            let manual = truthy(field("manual"));
            if (!truthy(task.get("supported")) && !manual)
                || !truthy(field("gameId"))
                || !seconds
                    .map(|value| value.is_finite() && (1.0..=86400.0).contains(&value))
                    .unwrap_or(false)
            {
                let reason = truthy_field(task, "reason")
                    .cloned()
                    .unwrap_or(json!("请选择准确的游戏和时长"));
                skipped.push(json!({"key": key, "reason": reason}));
                continue;
            }
            let platform = match task_platform(task) {
                Ok(platform) => platform,
                Err(reason) => {
                    skipped.push(json!({"key": key, "reason": reason}));
                    continue;
                }
            };
            let mapped_platform = field("platformId")
                .filter(|value| !value.is_null())
                .map_or(Some(PC_PLATFORM), Value::as_i64);
            if mapped_platform != Some(platform) {
                skipped.push(json!({"key": key, "reason": format!(
                    "游戏映射与任务要求的 {} 平台不符，请刷新活动或重新设置", platform_name(platform)
                )}));
                continue;
            }
            let seconds = seconds.unwrap_or(0.0);
            let area = field("areaId").filter(|value| truthy(Some(value)));
            let mut group_key = format!(
                "{}:{}",
                optional_id(field("gameId")),
                area.map(value_id).unwrap_or_else(|| "auto".to_owned())
            );
            if platform != PC_PLATFORM {
                group_key = format!("{platform}:{group_key}");
            }
            let index = match indices.get(&group_key) {
                Some(index) => *index,
                None => {
                    let index = groups.len();
                    let mut group =
                        json!({"id": group_key, "tasks": [], "seconds": 0, "platformId": platform});
                    for name in ["gameId", "areaId", "gameName"] {
                        insert_present(&mut group, name, field(name));
                    }
                    groups.push(group);
                    indices.insert(group_key, index);
                    index
                }
            };
            let group = &mut groups[index];
            let required = if number_is(task.get("status"), 2) {
                0.0
            } else {
                seconds
            };
            group["seconds"] = number_value(group["seconds"].as_f64().unwrap_or(0.0).max(required));
            let mut planned =
                json!({"key": key, "seconds": number_value(seconds), "manual": manual});
            for (output, source) in [
                ("activityId", "id"),
                ("activityName", "name"),
                ("endTime", "endTime"),
            ] {
                insert_present(&mut planned, output, activity.get(source));
            }
            for (output, source) in [("taskId", "id"), ("title", "title"), ("status", "status")] {
                insert_present(&mut planned, output, task.get(source));
            }
            if let Some(tasks) = group["tasks"].as_array_mut() {
                tasks.push(planned);
            }
        }
    }
    let total: f64 = groups
        .iter()
        .filter_map(|group| group["seconds"].as_f64())
        .sum();
    // 分组数量较少，稳定插入排序保留相同截止时间的顺序。
    // 缺失时间与 JS 的 NaN 一样不移动，避免非全序比较导致 Rust 排序器报错。
    let mut deadlines: Vec<f64> = groups.iter().map(deadline).collect();
    for index in 1..groups.len() {
        let mut position = index;
        while position > 0 && deadlines[position] < deadlines[position - 1] {
            groups.swap(position, position - 1);
            deadlines.swap(position, position - 1);
            position -= 1;
        }
    }
    json!({"groups": groups, "skipped": skipped, "totalSeconds": number_value(total)})
}

pub fn day_key(time: i64) -> String {
    let shanghai = FixedOffset::east_opt(8 * 3600).expect("上海时区偏移无效");
    Utc.timestamp_millis_opt(time)
        .single()
        .map(|date| date.with_timezone(&shanghai).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub fn acceleration_matches(
    status: &Value,
    game_id: i64,
    platform: i64,
    area_id: Option<i64>,
) -> bool {
    let actual_platform = status
        .get("platformId")
        .filter(|value| !value.is_null())
        .map_or(Some(PC_PLATFORM), Value::as_i64);
    status["active"] == true
        && number_is(status.get("gameId"), game_id)
        && actual_platform == Some(platform)
        && area_id.is_none_or(|area| number_is(status.get("areaId"), area))
}

pub fn valid_interval(
    previous: &Value,
    current: &Value,
    wall_delta_ms: f64,
    monotonic_delta_ms: f64,
    game_id: i64,
    max_gap: f64,
) -> f64 {
    if !truthy(previous.get("active"))
        || !truthy(current.get("active"))
        || !number_is(previous.get("gameId"), game_id)
        || !number_is(current.get("gameId"), game_id)
        || !wall_delta_ms.is_finite()
        || !monotonic_delta_ms.is_finite()
        || !max_gap.is_finite()
        || wall_delta_ms < 0.0
        || monotonic_delta_ms < 0.0
        || max_gap < 0.0
        || wall_delta_ms > max_gap
        || monotonic_delta_ms > max_gap
    {
        return 0.0;
    }
    wall_delta_ms.min(monotonic_delta_ms) / 1000.0
}

pub fn value_id(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                value.to_string()
            } else if let Some(value) = value.as_u64() {
                value.to_string()
            } else {
                let value = value.as_f64().unwrap_or(0.0);
                if value == 0.0 {
                    "0".to_owned()
                } else {
                    value.to_string()
                }
            }
        }
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    value_id(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

fn is_mobile(text: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(r"(?i)手机端|移动端|安卓|iOS(?:端|版|平台|$|[^a-z])|手游")
                .expect("手机任务正则表达式无效")
        })
        .is_match(text)
}

fn js_whitespace(c: char) -> bool {
    // ECMAScript 的空白包含 BOM，不包含 Unicode 的 NEL。
    (c.is_whitespace() && c != '\u{0085}') || c == '\u{feff}'
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_f64().map(|n| n != 0.0).unwrap_or(false),
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

fn truthy_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| truthy(Some(value)))
}

fn optional_id(value: Option<&Value>) -> String {
    value
        .map(value_id)
        .unwrap_or_else(|| "undefined".to_owned())
}

fn text_or_empty(value: Option<&Value>) -> String {
    if truthy(value) {
        optional_id(value)
    } else {
        String::new()
    }
}

fn insert_present(output: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        output[key] = value.clone();
    }
}

fn number_is(value: Option<&Value>, expected: i64) -> bool {
    match value {
        Some(Value::Number(value)) => {
            if let Some(integer) = value.as_i64() {
                integer == expected
            } else if let Some(integer) = value.as_u64() {
                expected >= 0 && integer == expected as u64
            } else {
                value.as_f64() == Some(expected as f64)
            }
        }
        _ => false,
    }
}

fn number_value(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 {
        if value >= i64::MIN as f64 && value < i64::MAX as f64 {
            return Value::from(value as i64);
        }
        if value >= 0.0 && value < u64::MAX as f64 {
            return Value::from(value as u64);
        }
    }
    Value::from(value)
}

pub(crate) fn js_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(value)) => value.as_f64().unwrap_or(f64::NAN),
        Some(value @ Value::String(_)) | Some(value @ Value::Array(_)) => {
            let text = value_id(value);
            let text = text.trim_matches(js_whitespace);
            if text.is_empty() {
                return 0.0;
            }
            for (prefixes, radix) in [(["0x", "0X"], 16), (["0b", "0B"], 2), (["0o", "0O"], 8)] {
                if let Some(digits) = prefixes
                    .iter()
                    .find_map(|prefix| text.strip_prefix(*prefix))
                {
                    if digits.is_empty() {
                        return f64::NAN;
                    }
                    return digits
                        .chars()
                        .try_fold(0.0, |number, c| {
                            c.to_digit(radix)
                                .map(|digit| number * f64::from(radix) + f64::from(digit))
                        })
                        .unwrap_or(f64::NAN);
                }
            }
            match text {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => {
                    static DECIMAL: OnceLock<Regex> = OnceLock::new();
                    let pattern = DECIMAL.get_or_init(|| {
                        Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$")
                            .expect("数值格式正则表达式无效")
                    });
                    if pattern.is_match(text) {
                        text.parse::<f64>().unwrap_or(f64::NAN)
                    } else {
                        f64::NAN
                    }
                }
            }
        }
        Some(Value::Object(_)) => f64::NAN,
    }
}

fn deadline(group: &Value) -> f64 {
    let mut earliest = f64::INFINITY;
    if let Some(tasks) = group["tasks"].as_array() {
        for task in tasks {
            let time = js_number(task.get("endTime"));
            if time.is_nan() {
                return f64::NAN;
            }
            earliest = earliest.min(time);
        }
    }
    earliest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    const NOW: i64 = 1_000_000;

    fn activity(id: &str, seconds: i64, status: i64) -> Value {
        json!({"id": id, "name": id, "startTime": 1, "endTime": NOW + 86400000,
            "tickets": 0, "tasks": [{"id": 1, "type": 7, "status": status,
                "supported": true, "title": "加速", "seconds": seconds}]})
    }

    fn mapping(pairs: &[(&str, i64)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(id, game)| {
                    (
                        format!("{id}:1"),
                        json!({"gameId": game, "gameName": "游戏"}),
                    )
                })
                .collect::<Map<String, Value>>(),
        )
    }

    #[test]
    fn activity_url_validates_origin_path_and_id() {
        assert_eq!(activity_id(&public_url("Aa_01-b")).unwrap(), "Aa_01-b");
        assert_eq!(
            activity_id(
                "https://WWW.BIUBIU001.COM:443/activity/activityTemplate?activityUuid=%61_1#rules"
            )
            .unwrap(),
            "a_1"
        );
        for url in [
            "https://example.org/activity/activityTemplate?activityUuid=x",
            "http://www.biubiu001.com/activity/activityTemplate?activityUuid=x",
            "https://www.biubiu001.com:444/activity/activityTemplate?activityUuid=x",
            "https://www.biubiu001.com.evil.test/activity/activityTemplate?activityUuid=x",
            "https://www.biubiu001.com./activity/activityTemplate?activityUuid=x",
            "https://www.biubiu001.com/activity/activityTemplate/?activityUuid=x",
            "https://www.biubiu001.com/activity/other?activityUuid=x",
            "https://www.biubiu001.com/activity/activityTemplate",
            "https://www.biubiu001.com/activity/activityTemplate?activityUuid=",
            "https://www.biubiu001.com/activity/activityTemplate?activityUuid=a%2Fb",
            "https://www.biubiu001.com/activity/activityTemplate?activityUuid=a+b",
            "https://www.biubiu001.com/activity/activityTemplate?activityUuid=中文",
            "/activity/activityTemplate?activityUuid=x",
        ] {
            assert!(activity_id(url).is_err(), "{url}");
        }
    }

    #[test]
    fn activity_url_rejects_all_userinfo() {
        for userinfo in ["user@", "user:pass@", ":pass@", ":@", "@", "user%40other@"] {
            let url = format!("https://{userinfo}www.biubiu001.com{ROUTE}?activityUuid=x");
            assert!(activity_id(&url).is_err(), "{url}");
        }
    }

    #[test]
    fn activity_url_uses_first_query_id() {
        let prefix = format!("{ORIGIN}{ROUTE}");
        assert_eq!(
            activity_id(&format!("{prefix}?activityUuid=first&activityUuid=second")).unwrap(),
            "first"
        );
        assert!(activity_id(&format!("{prefix}?activityUuid=&activityUuid=second")).is_err());
        assert_eq!(
            activity_id(&format!("{prefix}?other=a@b&activityUuid=x")).unwrap(),
            "x"
        );
    }

    #[test]
    fn public_url_encodes_like_encode_uri_component() {
        assert_eq!(
            public_url("a b/中+"),
            format!("{ORIGIN}{ROUTE}?activityUuid=a%20b%2F%E4%B8%AD%2B")
        );
        assert!(public_url("-_.!~*'()").ends_with("activityUuid=-_.!~*'()"));
    }

    #[test]
    fn initial_state_parses_multiline_json_data() {
        assert_eq!(
            parse_initial_state(
                "<script>window.__INITIAL_STATE__ = {\n\"actInfo\":{\"tasks\":[]}};\n</script>"
            )
            .unwrap(),
            json!({"actInfo":{"tasks":[]}})
        );
        assert_eq!(
            parse_initial_state(
                "<script>window.__INITIAL_STATE__ = {\"text\":\"}; keep\"};</script>"
            )
            .unwrap(),
            json!({"text":"}; keep"})
        );
        assert_eq!(
            parse_initial_state("<script>window.__INITIAL_STATE__=null;</script>").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn initial_state_rejects_executable_or_missing_data() {
        for html in [
            "<script>window.__INITIAL_STATE__ = alert(1);</script>",
            "<script>window.__INITIAL_STATE__ = {\"x\":1};alert(1);</script>",
            "<script>window.__INITIAL_STATE__ = {x:1};</script>",
            "<script>window.__INITIAL_STATE__ = {\"x\":1,};</script>",
            "<script>window.__INITIAL_STATE__ = {};",
            "<html></html>",
        ] {
            assert!(parse_initial_state(html).is_err(), "{html}");
        }
    }

    #[test]
    fn requirement_extracts_pc_durations_without_version_or_draw_counts() {
        for (title, name, seconds) in [
            ("加速《黎明杀机》10min", "黎明杀机", 600),
            ("每天加速赛马娘繁中版PC端超5分钟", "赛马娘繁中版", 300),
            ("加速COD系列游戏 10min", "COD", 600),
            ("每天加速街头霸王6 游戏 超10分钟", "街头霸王6", 600),
            ("加速PUBG 30min", "PUBG", 1800),
            ("加速《游戏2》1.5小时", "游戏2", 5400),
        ] {
            assert_eq!(
                task_requirement(&json!({"type":7,"title":title,"subTitle":"获得1次抽奖机会"})),
                json!({"supported":true,"gameName":name,"seconds":seconds,"platformId":6}),
                "{title}"
            );
        }
    }

    #[test]
    fn requirement_rejects_mobile_and_non_acceleration_tasks() {
        for suffix in ["手机端", "移动端", "安卓端", "iOS端", "IOS端", "手游端"] {
            assert_eq!(
                task_requirement(
                    &json!({"type":7,"title":format!("每天加速边狱公司{suffix}超10分钟")})
                )["supported"],
                false
            );
            assert_eq!(
                task_requirement(&json!({"type":7,"title":"加速《游戏》10min","subTitle":suffix}))
                    ["supported"],
                false
            );
        }
        for task in [
            json!({"type":11,"title":"游戏内累计生存时间30分钟"}),
            json!({"type":8,"title":"邀请好友加速10分钟"}),
            json!({"type":7,"title":"加速任意游戏"}),
            json!({"type":"7","title":"加速《游戏》10min"}),
        ] {
            assert_eq!(task_requirement(&task)["supported"], false);
        }
    }

    #[test]
    fn warframe_mobile_requirements_are_rejected_for_every_name_and_platform_hint() {
        for name in ["Warframe", "WARFRAME", "星际战甲", "星際戰甲"] {
            for suffix in [
                "手机端",
                "移动端",
                "安卓",
                "安卓端",
                "iOS",
                "iOS端",
                "手游",
                "手游端",
            ] {
                for title in [
                    format!("每天加速{name}{suffix}超10分钟"),
                    format!("加速《{name}》{suffix}10min"),
                    format!("加速《{name}{suffix}》10min"),
                ] {
                    let requirement = task_requirement(&json!({"type":7,"title":title}));
                    assert_eq!(
                        requirement,
                        json!({"supported":false,"reason":MOBILE_REASON}),
                        "{title}"
                    );
                }
            }
        }
    }

    #[test]
    fn warframe_pc_requirements_keep_the_original_search_name() {
        for name in ["Warframe", "WARFRAME", "星际战甲", "星際戰甲"] {
            assert_eq!(
                task_requirement(&json!({"type":7,"title":format!("加速《{name}》PC端5分钟")})),
                json!({"supported":true,"gameName":name,"seconds":300,"platformId":6})
            );
        }
    }

    #[test]
    fn mobile_requirements_cannot_be_overridden_by_console_platforms_or_reward_names() {
        for task in [
            json!({"type":7,"title":"加速《Warframe攻略》手机端10分钟"}),
            json!({"type":7,"title":"加速《其他游戏》手机端10分钟","subTitle":"Warframe 活动"}),
            json!({"type":7,"title":"加速其他游戏手机端10分钟","subTitle":"奖励来自《Warframe》"}),
            json!({"type":7,"title":"加速《Warframe》手机端 PS5 10分钟"}),
            json!({"type":7,"title":"加速《Warframe》手机端10分钟","platformId":9}),
            json!({"type":11,"title":"绑定《Warframe》手机端"}),
        ] {
            assert_eq!(task_requirement(&task)["supported"], false, "{task}");
            assert!(task_requirement(&task).get("sharedAcceleration").is_none());
        }
        for (hint, platform) in [("PS5", 8), ("Xbox", 9), ("Switch", 7), ("Steam Deck", 10)] {
            let requirement = task_requirement(
                &json!({"type":7,"title":format!("加速《Warframe》 {hint} 10分钟")}),
            );
            assert_eq!(requirement["supported"], true);
            assert_eq!(requirement["platformId"], platform);
            assert!(requirement.get("sharedAcceleration").is_none());
        }
        let invalid = task_requirement(&json!({"type":7,"title":"加速《Warframe》手机端0分钟"}));
        assert_eq!(invalid["supported"], false);
        assert_eq!(invalid["reason"], MOBILE_REASON);
    }

    #[test]
    fn mobile_cache_uses_task_description_and_preserves_reward_state() {
        let pc = json!({"id":101,"type":7,"title":"加速《星际战甲》PC端5分钟",
            "status":2,"finishCount":1,"supported":true,"gameName":"星际战甲","seconds":300,"platformId":6});
        let other = json!({"id":2,"type":7,"title":"加速《边狱公司》手机端10分钟",
            "status":1,"finishCount":0,"supported":false,"reason":MOBILE_REASON});
        let mut activities = [json!({"id":"a","tasks":[
            {"id":"mobile-202","type":7,"title":"Warframe","description":"加速《Warframe》移动端10分钟",
                "status":2,"finishCount":3,"supported":true,"gameName":"Warframe","seconds":600,
                "platformId":6,"sharedAcceleration":"warframe-pc"},
            pc,
            other
        ]})];
        normalize_task_platforms(&mut activities);
        let task = &activities[0]["tasks"][0];
        assert_eq!(task["id"], "mobile-202");
        assert_eq!(task["status"], 2);
        assert_eq!(task["finishCount"], 3);
        assert_eq!(task["supported"], false);
        assert_eq!(task["reason"], MOBILE_REASON);
        assert_eq!(task["gameName"], "Warframe");
        assert_eq!(task["seconds"], 600);
        assert_eq!(task["platformId"], 6);
        assert_eq!(task["sharedAcceleration"], "warframe-pc");
        assert_eq!(activities[0]["tasks"][1], pc);
        assert_eq!(activities[0]["tasks"][2], other);
        let normalized = activities.clone();
        normalize_task_platforms(&mut activities);
        assert_eq!(activities, normalized);
    }

    #[test]
    fn cached_mobile_reason_stays_disabled_without_task_description() {
        let task = json!({"id":"00202","type":7,"title":"每日奖励",
            "status":2,"finishCount":"4","supported":true,"seconds":600,"reason":MOBILE_REASON});
        let mut activities = [json!({"id":"a","tasks":[task]})];
        normalize_task_platforms(&mut activities);
        let task = &activities[0]["tasks"][0];
        assert_eq!(task["id"], "00202");
        assert_eq!(task["status"], 2);
        assert_eq!(task["finishCount"], "4");
        assert_eq!(task["supported"], false);
        assert_eq!(task["reason"], MOBILE_REASON);
        let plan = build_plan(
            &activities,
            &json!({"a:00202":{"gameId":42,"manual":true,"seconds":600}}),
            None,
            NOW,
        );
        assert_eq!(plan["groups"], json!([]));
        assert_eq!(plan["skipped"][0]["reason"], MOBILE_REASON);
    }

    #[test]
    fn legacy_sharing_marker_does_not_change_requirements_or_planning() {
        for name in ["Warframe", "星际战甲", "其他游戏"] {
            for (hint, platform) in [("PC端", 6), ("PS5端", 8), ("Xbox端", 9), ("手机端", 6)]
            {
                for status in [1, 2, 3] {
                    let task = json!({"id":"00202","type":7,"title":format!("加速《{name}》{hint}10分钟"),
                        "status":status,"finishCount":"4"});
                    let mut legacy = task.clone();
                    legacy["sharedAcceleration"] = json!("warframe-pc");
                    assert_eq!(task_requirement(&legacy), task_requirement(&task));
                    let mut activities = [json!({"id":"a","tasks":[task]})];
                    let mut marked = [json!({"id":"a","tasks":[legacy]})];
                    normalize_task_platforms(&mut activities);
                    normalize_task_platforms(&mut marked);
                    assert_eq!(marked[0]["tasks"][0]["sharedAcceleration"], "warframe-pc");
                    marked[0]["tasks"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("sharedAcceleration");
                    assert_eq!(marked, activities);
                    marked[0]["tasks"][0]["sharedAcceleration"] = json!("warframe-pc");
                    let mappings = json!({"a:00202":{"gameId":42,"platformId":platform,"manual":true,"seconds":600}});
                    let plan = build_plan(&marked, &mappings, None, NOW);
                    assert_eq!(plan, build_plan(&activities, &mappings, None, NOW));
                    assert_eq!(
                        plan["totalSeconds"],
                        if hint == "手机端" || status != 1 {
                            0
                        } else {
                            600
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn requirement_takes_maximum_duration_and_supports_units() {
        for (unit, multiplier) in [
            ("分钟", 60),
            ("MINUTES", 60),
            ("min", 60),
            ("小时", 3600),
            ("hour", 3600),
            ("H", 3600),
            ("秒", 1),
            ("秒钟", 1),
            ("SECONDS", 1),
            ("s", 1),
        ] {
            let task = json!({"type":7,"title":format!("加速《游戏2》2{unit}")});
            assert_eq!(task_requirement(&task)["seconds"], 2 * multiplier);
        }
        let task = json!({"type":7,"title":"累计加速《游戏》5分钟。加速《游戏》10min","subTitle":"获得99次抽奖机会"});
        assert_eq!(task_requirement(&task)["seconds"], 600);
        assert_eq!(
            task_requirement(&json!({"type":7,"title":"游戏","subTitle":"加速《另一游戏》3分钟"}))
                ["gameName"],
            "另一游戏"
        );
    }

    #[test]
    fn requirement_preserves_unresolved_fields_and_rejects_invalid_lengths() {
        assert_eq!(
            task_requirement(&json!({"type":7,"title":"加速任意游戏"})),
            json!({"supported":false,"reason":REQUIREMENT_REASON,"gameName":null,"seconds":null,"platformId":6})
        );
        for title in [
            "加速《游戏》0分钟",
            "加速《游戏》-5分钟",
            "加速《游戏》25小时",
        ] {
            assert_eq!(
                task_requirement(&json!({"type":7,"title":title}))["supported"],
                false
            );
        }
        assert_eq!(
            task_requirement(&json!({"type":7,"title":"加速《游戏》24小时"}))["seconds"],
            86400
        );
        assert_eq!(
            task_requirement(&json!({"type":7,"title":"加速1分钟"}))["supported"],
            false
        );
    }

    #[test]
    fn simplify_activity_preserves_json_fields_and_id_types() {
        let entry = json!({"activeLink":public_url("abc"),"activeName":"入口名","activeCoverImg":"入口封面","startTime":2,"endTime":3});
        let info = json!({"startTime":10,"endTime":20,"activeCoverImg":"详情封面","isOver":1,"lotteryTicket":"3",
        "freeGiftModule":{"id":1},"tasks":[
            {"taskId":123,"type":7,"status":2,"title":"加速《游戏》10min","subTitle":"获得1次抽奖机会","awardNum":2,"finishCount":1},
            {"taskId":"00123","type":7,"status":1,"title":"加速《游戏》5min"},
            {"type":11,"title":"游戏内累计生存时间30分钟"},
            {"taskId":0,"type":7,"title":"加速《游戏》1min"}
        ]});
        let result = simplify_activity(
            &entry,
            &info,
            &json!({"pageTitle":"配置名","ruleContent":["规则"]}),
        )
        .unwrap();
        assert_eq!(result["id"], "abc");
        assert_eq!(result["name"], "入口名");
        assert_eq!(result["cover"], "入口封面");
        assert_eq!(result["startTime"], 10);
        assert_eq!(result["endTime"], 20);
        assert_eq!(result["isOver"], true);
        assert_eq!(result["tickets"], 3);
        assert_eq!(result["rules"], json!(["规则"]));
        assert_eq!(result["freeGiftModule"], json!({"id":1}));
        assert_eq!(
            result["tasks"][0],
            json!({"id":123,"type":7,"status":2,"title":"加速《游戏》10min",
            "description":"获得1次抽奖机会","awardNum":2,"finishCount":1,"supported":true,"gameName":"游戏","seconds":600,"platformId":6})
        );
        assert_eq!(result["tasks"][1]["id"], "00123");
        assert_eq!(result["tasks"][2]["id"], "type-11");
        assert_eq!(result["tasks"][3]["id"], 0);
        assert!(result["tasks"][1].get("awardNum").is_none());
    }

    #[test]
    fn simplify_activity_fallbacks_and_missing_dates_match_js() {
        let entry = json!({"activeLink":public_url("a"),"startTime":2,"endTime":3});
        let result = simplify_activity(
            &entry,
            &json!({"startTime":0,"endTime":null,"activeCoverImg":"详情封面"}),
            &json!({"pageTitle":"配置名"}),
        )
        .unwrap();
        assert_eq!(result["name"], "配置名");
        assert_eq!(result["cover"], "详情封面");
        assert_eq!(result["startTime"], 2);
        assert_eq!(result["endTime"], 3);
        assert_eq!(result["tickets"], 0);
        assert_eq!(result["freeGiftModule"], Value::Null);
        let missing = simplify_activity(
            &json!({"activeLink":public_url("a")}),
            &json!({}),
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            missing,
            json!({"id":"a","name":"a","url":public_url("a"),"cover":"","isOver":false,
            "tickets":0,"rules":[],"freeGiftModule":null,"tasks":[]})
        );
    }

    #[test]
    fn simplify_activity_reports_invalid_url_or_tasks() {
        assert!(simplify_activity(&json!({}), &json!({}), &json!({})).is_err());
        let entry = json!({"activeLink":public_url("a")});
        for tasks in [json!({}), json!("wrong"), json!([null])] {
            assert!(simplify_activity(&entry, &json!({"tasks":tasks}), &json!({})).is_err());
        }
    }

    #[test]
    fn normalize_name_matches_js_punctuation_and_whitespace() {
        assert_eq!(
            normalize_name(" PUBG：《绝地 求生》（PC）-·(服)\n"),
            "pubg绝地求生pc服"
        );
        assert_eq!(normalize_name("\u{feff}A\u{00a0}B\u{3000}"), "ab");
        assert_eq!(normalize_name("A_B,+/服"), "a_b,+/服");
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn choose_game_does_not_guess_ambiguous_names() {
        let candidates = [
            json!({"gameInfo":{"gameId":1,"aliasName":"PUBG 测试服"}}),
            json!({"gameInfo":{"gameId":2,"aliasName":"PUBG 手机版"}}),
        ];
        assert!(choose_game("PUBG", &candidates).is_none());
        assert_eq!(
            choose_game("PUBG 测试服", &candidates).unwrap()["gameInfo"]["gameId"],
            1
        );
        assert!(choose_game("PUBG", &[]).is_none());
    }

    #[test]
    fn choose_game_prefers_primary_name_over_search_alias() {
        let candidates = [
            json!({"gameInfo":{"gameId":1,"aliasName":"绝地求生：测试服","aliasSearchName":"绝地求生"}}),
            json!({"gameInfo":{"gameId":2,"aliasName":"绝地求生"}}),
        ];
        assert_eq!(
            choose_game("绝地求生", &candidates).unwrap()["gameInfo"]["gameId"],
            2
        );
    }

    #[test]
    fn choose_game_handles_aliases_origin_names_and_single_result() {
        let candidates = [
            json!({"gameInfo":{"gameId":1,"gameName":"游戏一","originName":"Game One","aliasSearchName":"A,第一款"}}),
            json!({"gameInfo":{"gameId":2,"gameName":"游戏二","aliasSearchName":"B,第二款"}}),
        ];
        for name in ["游戏一", "GAME-ONE", "第一款", "a"] {
            assert_eq!(
                choose_game(name, &candidates).unwrap()["gameInfo"]["gameId"],
                1
            );
        }
        assert_eq!(
            choose_game("不匹配", &candidates[..1]).unwrap(),
            candidates[0]
        );
        let duplicate = [candidates[0].clone(), candidates[0].clone()];
        assert!(choose_game("游戏一", &duplicate).is_none());
    }

    #[test]
    fn requirement_distinguishes_console_tasks_from_the_pc_client() {
        for (suffix, platform) in [
            ("PS端", 8),
            ("Xbox端", 9),
            ("Switch端", 7),
            ("Steam Deck端", 10),
        ] {
            let task = json!({"type":7,"title":format!("加速《GTA6》{suffix}10min"),
                "subTitle":format!("使用biubiu加速器（PC端）累计加速《GTA6》{suffix}10min")});
            let requirement = task_requirement(&task);
            assert_eq!(requirement["supported"], true);
            assert_eq!(requirement["platformId"], platform);
            assert_eq!(requirement["gameName"], "GTA6");
            assert_eq!(requirement["seconds"], 600);
        }
        assert_eq!(
            task_platform(&json!({"title":"加速《Xbox PC Game Pass》PC端10min"})).unwrap(),
            6
        );
        assert!(task_platform(&json!({"title":"加速《GTA6》PS端或Xbox端10min"})).is_err());
        assert!(task_platform(&json!({"title":"加速《GTA6》主机端10min"})).is_err());
        assert_eq!(
            task_requirement(&json!({"type":7,"title":"加速《游戏》10min","description":"手机端"}))
                ["supported"],
            false
        );
    }

    #[test]
    fn cached_console_tasks_reject_legacy_pc_mappings_even_when_manual() {
        let mut tasks = vec![json!({"id":"gta","tasks":[
            {"id":1,"type":7,"status":1,"title":"加速《GTA6》PS端10min","supported":true,"seconds":600},
            {"id":2,"type":7,"status":1,"title":"加速《GTA6》Xbox端10min","supported":true,"seconds":600}
        ]})];
        normalize_task_platforms(&mut tasks);
        assert_eq!(tasks[0]["tasks"][0]["platformId"], 8);
        assert_eq!(tasks[0]["tasks"][1]["platformId"], 9);
        for manual in [false, true] {
            let plan = build_plan(
                &tasks,
                &json!({
                    "gta:1":{"gameId":11001121,"manual":manual},
                    "gta:2":{"gameId":11001121,"platformId":6,"manual":manual}
                }),
                None,
                NOW,
            );
            assert_eq!(plan["groups"], json!([]));
            assert_eq!(plan["skipped"].as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn console_platforms_have_separate_groups_and_durations() {
        let mut ps = activity("ps", 600, 1);
        ps["tasks"][0]["platformId"] = json!(8);
        let mut xbox = activity("xbox", 600, 1);
        xbox["tasks"][0]["platformId"] = json!(9);
        let plan = build_plan(
            &[ps, xbox],
            &json!({
                "ps:1":{"gameId":42,"platformId":8},
                "xbox:1":{"gameId":42,"platformId":9}
            }),
            None,
            NOW,
        );
        assert_eq!(plan["groups"].as_array().unwrap().len(), 2);
        assert_eq!(plan["groups"][0]["platformId"], 8);
        assert_eq!(plan["groups"][1]["platformId"], 9);
        assert_ne!(plan["groups"][0]["id"], plan["groups"][1]["id"]);
        assert_eq!(plan["totalSeconds"], 1200);
    }

    #[test]
    fn acceleration_requires_the_requested_platform_and_area() {
        let status = json!({"active":true,"gameId":42,"platformId":8,"areaId":7});
        assert!(acceleration_matches(&status, 42, 8, Some(7)));
        assert!(!acceleration_matches(&status, 42, 9, Some(7)));
        assert!(!acceleration_matches(&status, 42, 8, Some(8)));
        assert!(!acceleration_matches(&status, 41, 8, Some(7)));
        let legacy = json!({"active":true,"gameId":42});
        assert!(acceleration_matches(&legacy, 42, 6, None));
        assert!(!acceleration_matches(&legacy, 42, 8, None));
    }

    #[test]
    fn plan_shares_same_game_and_uses_maximum_not_sum() {
        let plan = build_plan(
            &[activity("a", 300, 1), activity("b", 600, 1)],
            &mapping(&[("a", 1), ("b", 1)]),
            None,
            NOW,
        );
        assert_eq!(plan["groups"].as_array().unwrap().len(), 1);
        assert_eq!(plan["totalSeconds"], 600);
        assert_eq!(plan["groups"][0]["id"], "1:auto");
        assert_eq!(plan["groups"][0]["tasks"].as_array().unwrap().len(), 2);
    }

    fn warframe_activity() -> Value {
        simplify_activity(
            &json!({"activeLink":public_url("warframe"),"activeName":"Warframe"}),
            &json!({"startTime":1,"endTime":NOW + 86400000,"tasks":[
                {"taskId":101,"type":7,"status":1,"title":"加速《Warframe》PC端5分钟"},
                {"taskId":"mobile-202","type":7,"status":1,"title":"加速《Warframe》手机端10分钟"}
            ]}),
            &json!({}),
        )
        .unwrap()
    }

    #[test]
    fn warframe_plan_keeps_only_pc_rewards_with_original_ids_and_duration() {
        let plan = build_plan(
            &[warframe_activity()],
            &json!({
                "warframe:101":{"gameId":42,"gameName":"Warframe","platformId":6},
                "warframe:mobile-202":{"gameId":42,"gameName":"Warframe","platformId":6}
            }),
            None,
            NOW,
        );
        assert_eq!(plan["groups"].as_array().unwrap().len(), 1);
        assert_eq!(plan["totalSeconds"], 300);
        let group = &plan["groups"][0];
        assert_eq!(group["platformId"], 6);
        assert_eq!(group["gameId"], 42);
        assert_eq!(group["tasks"][0]["taskId"], 101);
        assert_eq!(group["tasks"][0]["seconds"], 300);
        assert_eq!(group["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(
            plan["skipped"],
            json!([{"key":"warframe:mobile-202","reason":MOBILE_REASON}])
        );
    }

    #[test]
    fn warframe_mobile_description_survives_simplification_cache_and_planning() {
        for (subtitle, description) in [
            ("", "加速《Warframe》手机端10分钟"),
            ("每日奖励", "加速《Warframe》手机端10分钟"),
            ("加速《Warframe》手机端10分钟", "每日奖励"),
        ] {
            let mut activity = simplify_activity(
                &json!({"activeLink":public_url("warframe")}),
                &json!({"tasks":[{
                    "taskId":"mobile-202","type":7,"status":1,"title":"每天累计加速10分钟",
                    "subTitle":subtitle,"description":description
                }]}),
                &json!({}),
            )
            .unwrap();
            let task = &activity["tasks"][0];
            assert_eq!(task["supported"], false);
            assert_eq!(task["reason"], MOBILE_REASON);
            assert!(task.get("sharedAcceleration").is_none());
            assert!(task["description"].as_str().unwrap().contains("Warframe"));
            normalize_task_platforms(std::slice::from_mut(&mut activity));
            let plan = build_plan(
                &[activity],
                &json!({"warframe:mobile-202":{"gameId":42,"platformId":6,"manual":true,"seconds":600}}),
                None,
                NOW,
            );
            assert_eq!(plan["totalSeconds"], 0);
            assert_eq!(plan["groups"], json!([]));
            assert_eq!(
                plan["skipped"],
                json!([{"key":"warframe:mobile-202","reason":MOBILE_REASON}])
            );
        }
    }

    #[test]
    fn warframe_mobile_ready_reward_stays_excluded_after_pc_is_claimed() {
        let mut activity = warframe_activity();
        activity["tasks"][0]["status"] = json!(3);
        activity["tasks"][1]["status"] = json!(2);
        activity["tasks"][1]["supported"] = json!(true);
        activity["tasks"][1]["sharedAcceleration"] = json!("warframe-pc");
        let plan = build_plan(
            &[activity],
            &json!({"warframe:mobile-202":{"gameId":42,"platformId":6,"manual":true,"seconds":600}}),
            None,
            NOW,
        );
        assert_eq!(plan["totalSeconds"], 0);
        assert_eq!(plan["groups"], json!([]));
        assert_eq!(
            plan["skipped"][1],
            json!({"key":"warframe:mobile-202","reason":MOBILE_REASON})
        );
    }

    #[test]
    fn warframe_mobile_tasks_reject_legacy_manual_mappings_on_every_platform_and_status() {
        for status in [1, 2, 3] {
            for platform in [1, 6, 7, 8, 9, 10] {
                for manual in [false, true] {
                    let mut activity = warframe_activity();
                    activity["tasks"][1]["status"] = json!(status);
                    activity["tasks"][1]["supported"] = json!(true);
                    activity["tasks"][1]["seconds"] = json!(600);
                    activity["tasks"][1]["sharedAcceleration"] = json!("warframe-pc");
                    let plan = build_plan(
                        &[activity],
                        &json!({"warframe:mobile-202":{"gameId":42,"platformId":platform,
                            "manual":manual,"seconds":600}}),
                        None,
                        NOW,
                    );
                    assert_eq!(plan["groups"], json!([]));
                    assert_eq!(plan["totalSeconds"], 0);
                    assert_eq!(plan["skipped"][1]["reason"], MOBILE_REASON);
                }
            }
        }
    }

    #[test]
    fn plan_excludes_claimed_expired_and_keeps_areas_separate() {
        let mut old = activity("old", 600, 1);
        old["endTime"] = json!(1);
        let plan = build_plan(
            &[
                activity("a", 600, 1),
                activity("b", 600, 1),
                activity("done", 600, 3),
                old,
            ],
            &json!({"a:1":{"gameId":1,"areaId":10},"b:1":{"gameId":1,"areaId":20},"done:1":{"gameId":2},"old:1":{"gameId":3}}),
            None,
            NOW,
        );
        assert_eq!(plan["groups"].as_array().unwrap().len(), 2);
        assert_eq!(plan["totalSeconds"], 1200);
        assert_eq!(
            plan["skipped"],
            json!([{"key":"done:1","reason":"服务器显示已领取"},{"activityId":"old","reason":"活动未开始或已结束"}])
        );
    }

    #[test]
    fn plan_ready_tasks_have_zero_acceleration_but_keep_claim_details() {
        let plan = build_plan(&[activity("a", 600, 2)], &mapping(&[("a", 1)]), None, NOW);
        assert_eq!(plan["totalSeconds"], 0);
        assert_eq!(plan["groups"][0]["seconds"], 0);
        assert_eq!(plan["groups"][0]["tasks"][0]["seconds"], 600);
        assert_eq!(plan["groups"][0]["tasks"][0]["status"], 2);
        let mixed = build_plan(
            &[activity("a", 1800, 2), activity("b", 300, 1)],
            &mapping(&[("a", 1), ("b", 1)]),
            None,
            NOW,
        );
        assert_eq!(mixed["totalSeconds"], 300);
    }

    #[test]
    fn plan_respects_selection_and_start_end_boundaries() {
        let mut future = activity("future", 600, 1);
        future["startTime"] = json!(NOW + 1);
        let mut ended = activity("ended", 600, 1);
        ended["endTime"] = json!(NOW);
        let mut over = activity("over", 600, 1);
        over["isOver"] = json!(true);
        let mut started = activity("started", 600, 1);
        started["startTime"] = json!(NOW);
        let activities = [future, ended, over, started];
        let mappings = mapping(&[("future", 1), ("ended", 2), ("over", 3), ("started", 4)]);
        let plan = build_plan(&activities, &mappings, None, NOW);
        assert_eq!(plan["groups"].as_array().unwrap().len(), 1);
        assert_eq!(plan["skipped"].as_array().unwrap().len(), 3);
        assert_eq!(
            build_plan(&activities, &mappings, Some(&[]), NOW),
            json!({"groups":[],"skipped":[],"totalSeconds":0})
        );
        let selected = ["started".to_owned()];
        let selected_plan = build_plan(&activities, &mappings, Some(&selected), NOW);
        assert_eq!(selected_plan["totalSeconds"], 600);
        assert_eq!(selected_plan["skipped"], json!([]));
    }

    #[test]
    fn plan_does_not_exclude_cache_errors_or_missing_dates() {
        let errored = json!({"id":"error","name":"错误缓存","tasks":[],"error":"读取失败"});
        let mut missing = activity("a", 600, 1);
        missing.as_object_mut().unwrap().remove("startTime");
        missing.as_object_mut().unwrap().remove("endTime");
        missing["error"] = json!("旧缓存");
        let plan = build_plan(&[errored, missing], &mapping(&[("a", 1)]), None, NOW);
        assert_eq!(plan["skipped"], json!([]));
        assert_eq!(plan["totalSeconds"], 600);
        assert!(plan["groups"][0]["tasks"][0].get("endTime").is_none());
        let mut explicit_null = activity("a", 600, 1);
        explicit_null["endTime"] = Value::Null;
        assert_eq!(
            build_plan(&[explicit_null], &mapping(&[("a", 1)]), None, NOW)["groups"],
            json!([])
        );
    }

    #[test]
    fn plan_coerces_numeric_date_strings_like_js() {
        let mut a = activity("a", 600, 1);
        a["startTime"] = json!("1");
        a["endTime"] = json!((NOW + 1).to_string());
        assert_eq!(
            build_plan(&[a.clone()], &mapping(&[("a", 1)]), None, NOW)["totalSeconds"],
            600
        );
        a["endTime"] = json!(NOW.to_string());
        assert_eq!(
            build_plan(&[a.clone()], &mapping(&[("a", 1)]), None, NOW)["totalSeconds"],
            0
        );
        a["endTime"] = json!("invalid date");
        assert_eq!(
            build_plan(&[a], &mapping(&[("a", 1)]), None, NOW)["totalSeconds"],
            600
        );
    }

    #[test]
    fn plan_manual_mapping_and_nullish_duration_override() {
        let mut a = activity("a", 600, 1);
        a["tasks"][0]["supported"] = json!(false);
        let plan = build_plan(
            &[a],
            &json!({"a:1":{"gameId":1,"seconds":90,"manual":true}}),
            None,
            NOW,
        );
        assert_eq!(plan["totalSeconds"], 90);
        assert_eq!(plan["groups"][0]["tasks"][0]["manual"], true);
        let plan = build_plan(
            &[activity("a", 600, 1)],
            &json!({"a:1":{"gameId":1,"seconds":null}}),
            None,
            NOW,
        );
        assert_eq!(plan["totalSeconds"], 600);
    }

    #[test]
    fn plan_rejects_invalid_duration_or_missing_mapping() {
        for seconds in [
            json!(0),
            json!(-1),
            json!(86401),
            json!("600"),
            json!(true),
            json!({}),
        ] {
            let plan = build_plan(
                &[activity("a", 600, 1)],
                &json!({"a:1":{"gameId":1,"seconds":seconds}}),
                None,
                NOW,
            );
            assert_eq!(plan["groups"], json!([]));
            assert_eq!(plan["skipped"].as_array().unwrap().len(), 1);
        }
        assert_eq!(
            build_plan(&[activity("a", 600, 1)], &json!({}), None, NOW)["groups"],
            json!([])
        );
        let mut unsupported = activity("a", 600, 1);
        unsupported["tasks"][0]["supported"] = json!(false);
        unsupported["tasks"][0]["reason"] = json!("需要手动设置");
        let plan = build_plan(&[unsupported], &mapping(&[("a", 1)]), None, NOW);
        assert_eq!(plan["skipped"][0]["reason"], "需要手动设置");
    }

    #[test]
    fn plan_mobile_tasks_stay_excluded_after_manual_mapping() {
        for field in ["title", "description", "subTitle"] {
            for mobile in ["手机端", "移动端", "安卓", "ios", "手游"] {
                for status in [1, 2, 3] {
                    let mut a = activity("a", 600, status);
                    a["tasks"][0][field] = json!(format!("加速游戏{mobile}10分钟"));
                    let plan = build_plan(
                        &[a],
                        &json!({"a:1":{"gameId":1,"manual":true,"seconds":600}}),
                        None,
                        NOW,
                    );
                    assert_eq!(plan["groups"], json!([]));
                    assert_eq!(plan["skipped"][0]["reason"], MOBILE_REASON);
                }
            }
        }
    }

    #[test]
    fn plan_preserves_numeric_and_string_task_ids() {
        for task_id in [json!(42), json!(42.0), json!("42"), json!("0042")] {
            let mut a = activity("a", 600, 1);
            a["tasks"][0]["id"] = task_id.clone();
            let key = format!("a:{}", value_id(&task_id));
            let mappings =
                Value::Object([(key.clone(), json!({"gameId":1}))].into_iter().collect());
            let plan = build_plan(&[a], &mappings, None, NOW);
            assert_eq!(plan["groups"][0]["tasks"][0]["taskId"], task_id);
            assert_eq!(plan["groups"][0]["tasks"][0]["key"], key);
        }
        assert_eq!(value_id(&json!(42.0)), "42");
        assert_eq!(value_id(&json!("0042")), "0042");
        assert_eq!(value_id(&json!(u64::MAX)), u64::MAX.to_string());
        assert_eq!(value_id(&json!(-0.0)), "0");
    }

    #[test]
    fn plan_uses_earliest_deadline_and_stable_order_for_ties() {
        let mut a = activity("a", 600, 1);
        let mut b = activity("b", 600, 1);
        let mut c = activity("c", 300, 1);
        let mut d = activity("d", 600, 1);
        a["endTime"] = json!(NOW + 900);
        b["endTime"] = json!(NOW + 100);
        c["endTime"] = json!(NOW + 50);
        d["endTime"] = json!(NOW + 100);
        let plan = build_plan(
            &[b, a, c, d],
            &mapping(&[("a", 1), ("b", 2), ("c", 1), ("d", 3)]),
            None,
            NOW,
        );
        let ids: Vec<&str> = plan["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["1:auto", "2:auto", "3:auto"]);
        assert_eq!(plan["totalSeconds"], 1800);
    }

    #[test]
    fn plan_merges_zero_and_missing_area_and_ignores_other_task_types() {
        let mut other = activity("other", 600, 1);
        other["tasks"][0]["type"] = json!(8);
        let plan = build_plan(
            &[activity("a", 300, 1), activity("b", 600, 1), other],
            &json!({"a:1":{"gameId":1,"areaId":0},"b:1":{"gameId":1},"other:1":{"gameId":2}}),
            None,
            NOW,
        );
        assert_eq!(plan["groups"].as_array().unwrap().len(), 1);
        assert_eq!(plan["totalSeconds"], 600);
        assert_eq!(plan["skipped"], json!([]));
    }

    #[test]
    fn verified_time_rejects_sleep_disconnects_and_game_changes() {
        let active = json!({"active":true,"gameId":1});
        assert_eq!(
            valid_interval(&active, &active, 5000.0, 5000.0, 1, 15000.0),
            5.0
        );
        assert_eq!(
            valid_interval(&active, &active, 600000.0, 5000.0, 1, 15000.0),
            0.0
        );
        assert_eq!(
            valid_interval(
                &active,
                &json!({"active":false}),
                5000.0,
                5000.0,
                1,
                15000.0
            ),
            0.0
        );
        assert_eq!(
            valid_interval(
                &active,
                &json!({"active":true,"gameId":2}),
                5000.0,
                5000.0,
                1,
                15000.0
            ),
            0.0
        );
        assert_eq!(
            valid_interval(&Value::Null, &active, 5000.0, 5000.0, 1, 15000.0),
            0.0
        );
        assert_eq!(
            valid_interval(
                &json!({"active":true,"gameId":"1"}),
                &active,
                5000.0,
                5000.0,
                1,
                15000.0
            ),
            0.0
        );
    }

    #[test]
    fn verified_time_takes_minimum_and_accepts_max_gap_boundary() {
        let active = json!({"active":true,"gameId":1});
        assert_eq!(
            valid_interval(&active, &active, 5000.0, 4500.0, 1, 15000.0),
            4.5
        );
        assert_eq!(
            valid_interval(&active, &active, 4500.0, 5000.0, 1, 15000.0),
            4.5
        );
        assert_eq!(
            valid_interval(&active, &active, 15000.0, 15000.0, 1, 15000.0),
            15.0
        );
        assert_eq!(
            valid_interval(&active, &active, 15001.0, 5000.0, 1, 15000.0),
            0.0
        );
        assert_eq!(
            valid_interval(&active, &active, 5000.0, 15001.0, 1, 15000.0),
            0.0
        );
        assert_eq!(valid_interval(&active, &active, 0.0, 0.0, 1, 0.0), 0.0);
    }

    #[test]
    fn verified_time_rejects_negative_and_non_finite_deltas() {
        let active = json!({"active":true,"gameId":1});
        for delta in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                valid_interval(&active, &active, delta, 5000.0, 1, 15000.0),
                0.0
            );
            assert_eq!(
                valid_interval(&active, &active, 5000.0, delta, 1, 15000.0),
                0.0
            );
        }
        assert_eq!(
            valid_interval(&active, &active, 5000.0, 5000.0, 1, f64::NAN),
            0.0
        );
    }

    #[test]
    fn day_key_uses_shanghai_midnight_instead_of_host_timezone() {
        let before = chrono::DateTime::parse_from_rfc3339("2026-11-07T15:59:59Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(day_key(before), "2026-11-07");
        assert_eq!(day_key(before + 999), "2026-11-07");
        assert_eq!(day_key(before + 1000), "2026-11-08");
    }

    #[test]
    fn day_key_handles_epoch_negative_times_and_year_rollover() {
        assert_eq!(day_key(0), "1970-01-01");
        assert_eq!(day_key(-8 * 3600 * 1000 - 1), "1969-12-31");
        let rollover = chrono::DateTime::parse_from_rfc3339("2026-12-31T16:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(day_key(rollover), "2027-01-01");
    }

    #[test]
    fn clock_returns_epoch_milliseconds() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let value = now_ms();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(value >= before && value <= after);
    }
}
