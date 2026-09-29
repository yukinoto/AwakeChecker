use serde_json::{Value, json};

use crate::{
    live::LiveSnapshot,
    model::{Scope, Segment, clip_text, format_rfc3339, overlap_online},
};

pub const TOOL_STATUS: &str = "get_status";
pub const TOOL_SESSIONS: &str = "list_sessions";
pub const TOOL_SUMMARY: &str = "summarize_presence";

pub fn tool_allowed(scopes: &[Scope], tool: &str) -> bool {
    match tool {
        TOOL_STATUS => scopes.contains(&Scope::Status),
        TOOL_SESSIONS => scopes.contains(&Scope::History),
        TOOL_SUMMARY => scopes.contains(&Scope::Summary),
        _ => false,
    }
}

pub fn visible_names(scopes: &[Scope]) -> Vec<&'static str> {
    let mut names = Vec::new();
    if scopes.contains(&Scope::Status) {
        names.push(Scope::Status.as_str());
    }
    if scopes.contains(&Scope::Since) {
        names.push(Scope::Since.as_str());
    }
    if scopes.contains(&Scope::Idle) {
        names.push(Scope::Idle.as_str());
    }
    names
}

pub fn project_status(snapshot: &LiveSnapshot, scopes: &[Scope]) -> Result<Value, &'static str> {
    if !scopes.contains(&Scope::Status) {
        return Err("status");
    }
    let mut body = json!({
        "visible": visible_names(scopes),
        "online": snapshot.online,
    });
    if scopes.contains(&Scope::Since) {
        body["since"] = json!(format_rfc3339(snapshot.since_ms));
    }
    if scopes.contains(&Scope::Idle) {
        body["idle_secs"] = json!(snapshot.idle_secs);
        body["locked"] = json!(snapshot.locked);
    }
    Ok(body)
}

pub fn project_sessions(segments: &[Segment], limit: usize) -> Value {
    let sessions: Vec<Value> = segments
        .iter()
        .take(limit)
        .map(|segment| {
            json!({
                "state": segment.state.as_str(),
                "started_at": format_rfc3339(segment.started_at),
                "ended_at": segment.ended_at.map(format_rfc3339),
                "end_reason": segment.end_reason.map(crate::model::EndReason::as_str),
            })
        })
        .collect();
    json!({ "sessions": sessions, "count": sessions.len() })
}

pub fn project_summary(segments: &[Segment], from_ms: i64, to_ms: i64, now: i64) -> Value {
    let (online_secs, segment_count) = overlap_online(segments, from_ms, to_ms, now);
    json!({
        "from": format_rfc3339(from_ms),
        "to": format_rfc3339(to_ms),
        "online_secs": online_secs,
        "segment_count": segment_count,
    })
}

pub fn args_summary(arguments: Option<&serde_json::Map<String, Value>>) -> String {
    match arguments {
        Some(map) if !map.is_empty() => clip_text(&Value::Object(map.clone()).to_string(), 180),
        _ => String::new(),
    }
}

pub fn result_summary(value: &Value) -> String {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return clip_text(error, 180);
    }
    if value.get("online").is_some() {
        let visible = value
            .get("visible")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        return format!(
            "online={} visible={visible}",
            value.get("online").and_then(Value::as_bool).unwrap_or(false)
        );
    }
    if let Some(count) = value.get("count").and_then(Value::as_u64) {
        return format!("sessions={count}");
    }
    if let Some(secs) = value.get("online_secs").and_then(Value::as_i64) {
        let segments = value.get("segment_count").and_then(Value::as_u64).unwrap_or(0);
        return format!("online_secs={secs} segments={segments}");
    }
    clip_text(&value.to_string(), 180)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EndReason, Presence, Segment};

    #[test]
    fn summary_scope_cannot_list_sessions() {
        let scopes = [Scope::Summary];
        assert!(tool_allowed(&scopes, TOOL_SUMMARY));
        assert!(!tool_allowed(&scopes, TOOL_SESSIONS));
        assert!(!tool_allowed(&scopes, TOOL_STATUS));
    }

    #[test]
    fn status_hides_idle_and_since_without_those_scopes() {
        let snapshot = LiveSnapshot {
            online: true,
            idle_secs: 12,
            locked: false,
            since_ms: 1_700_000_000_000,
        };
        let value = project_status(&snapshot, &[Scope::Status]).unwrap();
        assert_eq!(value["online"], json!(true));
        assert!(value.get("idle_secs").is_none());
        assert!(value.get("since").is_none());
        assert!(project_status(&snapshot, &[Scope::History]).is_err());
    }

    #[test]
    fn denied_summary_text_is_short() {
        let value = json!({"error": "permission denied: history"});
        assert_eq!(result_summary(&value), "permission denied: history");
        let sessions = vec![Segment {
            id: 1,
            state: Presence::Online,
            started_at: 0,
            ended_at: Some(5_000),
            end_reason: Some(EndReason::Idle),
            last_seen_at: 5_000,
        }];
        let summary = project_summary(&sessions, 0, 10_000, 10_000);
        assert_eq!(summary["online_secs"], json!(5));
        assert!(summary.get("sessions").is_none());
    }
}
