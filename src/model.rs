use chrono::{DateTime, Local, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

pub const STALE_GAP_MS: i64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Offline,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "online" => Some(Self::Online),
            "offline" => Some(Self::Offline),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Idle,
    Locked,
    Policy,
    Stopped,
    Returned,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Locked => "locked",
            Self::Policy => "policy",
            Self::Stopped => "stopped",
            Self::Returned => "returned",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "locked" => Some(Self::Locked),
            "policy" => Some(Self::Policy),
            "stopped" => Some(Self::Stopped),
            "returned" => Some(Self::Returned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Status,
    Since,
    Idle,
    History,
    Summary,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Since => "since",
            Self::Idle => "idle",
            Self::History => "history",
            Self::Summary => "summary",
        }
    }

    pub fn parse_list(values: &[String]) -> Result<Vec<Self>, String> {
        let mut scopes = Vec::with_capacity(values.len());
        for value in values {
            let scope = match value.as_str() {
                "status" => Self::Status,
                "since" => Self::Since,
                "idle" => Self::Idle,
                "history" => Self::History,
                "summary" => Self::Summary,
                other => return Err(format!("未知范围 {other}")),
            };
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        Ok(scopes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub idle_threshold_secs: u64,
    pub locked_is_offline: bool,
    pub online_confirm_secs: u64,
    pub sample_interval_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            idle_threshold_secs: 180,
            locked_is_offline: true,
            online_confirm_secs: 5,
            sample_interval_secs: 2,
        }
    }
}

impl Policy {
    pub fn validate_knobs(
        idle_threshold_secs: u64,
        online_confirm_secs: u64,
    ) -> Result<(), String> {
        if !(10..=86_400).contains(&idle_threshold_secs) {
            return Err("空闲阈值需要在 10 秒到 24 小时之间".into());
        }
        if online_confirm_secs > 3_600 {
            return Err("回到电脑后的确认时间不能超过 1 小时".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub id: i64,
    pub state: Presence,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<EndReason>,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLog {
    pub id: i64,
    pub client_id: String,
    pub client_name: String,
    pub tool: String,
    pub allowed: bool,
    pub args_summary: String,
    pub result_summary: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub at_ms: i64,
    pub idle_secs: u64,
    pub locked: bool,
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn format_rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|utc| {
            utc.with_timezone(&Local)
                .to_rfc3339_opts(SecondsFormat::Secs, false)
        })
        .unwrap_or_default()
}

pub fn start_of_today_ms() -> i64 {
    let today = Local::now().date_naive();
    start_of_local_day(today).unwrap_or_else(now_ms)
}

pub fn validate_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let chars = name.chars().count();
    if !(1..=40).contains(&chars) {
        return Err("名称需要 1 到 40 个字符".into());
    }
    if name.chars().any(|ch| ch.is_control()) {
        return Err("名称不能包含控制字符".into());
    }
    Ok(name.to_string())
}

/// `from`/`to` accept `YYYY-MM-DD` (local day) or RFC3339.
/// A date-only `to` is exclusive of the next local midnight, so that day is included.
pub fn parse_range_bound(value: &str, is_end: bool) -> Result<i64, String> {
    let value = value.trim();
    if value.len() == 10 && value.as_bytes().get(4) == Some(&b'-') {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("无法解析日期 {value}"))?;
        let date = if is_end {
            date.checked_add_signed(chrono::TimeDelta::days(1))
                .ok_or_else(|| format!("日期 {value} 超出范围"))?
        } else {
            date
        };
        return start_of_local_day(date).ok_or_else(|| format!("无法换算日期 {value}"));
    }
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp_millis())
        .map_err(|_| format!("无法解析时间 {value}"))
}

pub fn default_range(now: i64) -> (i64, i64) {
    (start_of_today_ms(), now)
}

fn start_of_local_day(date: NaiveDate) -> Option<i64> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    naive
        .and_local_timezone(Local)
        .earliest()
        .map(|dt| dt.timestamp_millis())
}

pub fn overlap_online(segments: &[Segment], from_ms: i64, to_ms: i64, now_ms: i64) -> (i64, u32) {
    let mut total_ms = 0i64;
    let mut count = 0u32;
    for segment in segments {
        if segment.state != Presence::Online {
            continue;
        }
        let end = segment.ended_at.unwrap_or(now_ms).min(to_ms);
        let start = segment.started_at.max(from_ms);
        if end > start {
            total_ms += end - start;
            count += 1;
        }
    }
    (total_ms / 1000, count)
}

pub fn clip_text(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let mut clipped: String = value.chars().take(max_chars).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_only_end_includes_that_local_day() {
        let start = parse_range_bound("2026-09-29", false).unwrap();
        let end = parse_range_bound("2026-09-29", true).unwrap();
        assert!(end - start >= 23 * 3_600_000);
        assert!(end - start <= 25 * 3_600_000);
    }

    #[test]
    fn overlap_clips_open_segment_to_now() {
        let segments = vec![Segment {
            id: 1,
            state: Presence::Online,
            started_at: 0,
            ended_at: None,
            end_reason: None,
            last_seen_at: 5_000,
        }];
        let (secs, count) = overlap_online(&segments, 0, 10_000, 4_000);
        assert_eq!(secs, 4);
        assert_eq!(count, 1);
    }

    #[test]
    fn overlap_ignores_offline_segments_and_subsecond_slivers() {
        let segments = vec![
            Segment {
                id: 1,
                state: Presence::Online,
                started_at: 0,
                ended_at: Some(1_000),
                end_reason: Some(EndReason::Idle),
                last_seen_at: 1_000,
            },
            Segment {
                id: 2,
                state: Presence::Offline,
                started_at: 1_000,
                ended_at: Some(5_000),
                end_reason: Some(EndReason::Returned),
                last_seen_at: 5_000,
            },
            Segment {
                id: 3,
                state: Presence::Online,
                started_at: 5_000,
                ended_at: Some(5_400),
                end_reason: Some(EndReason::Locked),
                last_seen_at: 5_400,
            },
        ];
        let (secs, count) = overlap_online(&segments, 500, 2_000, 9_000);
        assert_eq!(secs, 0);
        assert_eq!(count, 1);
        let (secs, count) = overlap_online(&segments, 10_000, 20_000, 20_000);
        assert_eq!((secs, count), (0, 0));
    }

    #[test]
    fn presence_and_reasons_round_trip() {
        assert_eq!(Presence::Online.as_str(), "online");
        assert_eq!(Presence::parse("offline"), Some(Presence::Offline));
        assert_eq!(Presence::parse("Online"), None);
        for reason in [
            EndReason::Idle,
            EndReason::Locked,
            EndReason::Policy,
            EndReason::Stopped,
            EndReason::Returned,
        ] {
            assert_eq!(EndReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(EndReason::parse("asleep"), None);
    }

    #[test]
    fn scope_list_rejects_unknown_and_drops_duplicates() {
        let scopes = Scope::parse_list(&[
            "history".to_string(),
            "status".to_string(),
            "status".to_string(),
        ])
        .unwrap();
        assert_eq!(scopes, vec![Scope::History, Scope::Status]);
        assert!(Scope::parse_list(&["nope".to_string()]).is_err());
        assert!(Scope::parse_list(&[]).unwrap().is_empty());
    }

    #[test]
    fn policy_knobs_stay_inside_the_documented_limits() {
        assert!(Policy::validate_knobs(10, 0).is_ok());
        assert!(Policy::validate_knobs(86_400, 3_600).is_ok());
        assert!(Policy::validate_knobs(9, 0).unwrap_err().contains("空闲"));
        assert!(Policy::validate_knobs(86_401, 0).is_err());
        assert!(
            Policy::validate_knobs(10, 3_601)
                .unwrap_err()
                .contains("1 小时")
        );
    }

    #[test]
    fn names_are_trimmed_and_reject_control_characters() {
        assert_eq!(validate_name("  座位  ").unwrap(), "座位");
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a\u{0001}b").is_err());
        let ok = "测".repeat(40);
        assert_eq!(validate_name(&ok).unwrap(), ok);
        assert!(validate_name(&"测".repeat(41)).is_err());
    }

    #[test]
    fn clip_text_counts_characters() {
        assert_eq!(clip_text("abc", 3), "abc");
        assert_eq!(clip_text("abcd", 3), "abc…");
        assert_eq!(clip_text("你好世界", 2), "你好…");
        assert_eq!(clip_text("a", 0), "…");
    }

    #[test]
    fn range_bounds_accept_rfc3339_and_reject_garbage() {
        let ms = parse_range_bound("  2026-01-01T00:00:00Z ", false).unwrap();
        assert_eq!(
            ms,
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert!(parse_range_bound("2026-13-40", false).is_err());
        assert!(parse_range_bound("nope", true).is_err());
        let formatted = format_rfc3339(1_700_000_000_000);
        assert_eq!(
            DateTime::parse_from_rfc3339(&formatted)
                .unwrap()
                .timestamp_millis(),
            1_700_000_000_000
        );
        assert_eq!(format_rfc3339(i64::MAX), "");
        let now = 1_700_000_000_000;
        assert_eq!(default_range(now).1, now);
        assert_eq!(default_range(now).0, start_of_today_ms());
    }
}
