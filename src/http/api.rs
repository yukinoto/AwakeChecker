use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    http::AppState,
    mcp::{issue_token, new_id},
    model::{
        Client, Policy, Scope, Segment, now_ms, overlap_online, parse_range_bound,
        start_of_today_ms, validate_name,
    },
};

#[derive(Serialize)]
struct PolicyBody {
    idle_threshold_secs: u64,
    locked_is_offline: bool,
    online_confirm_secs: u64,
}

#[derive(Deserialize)]
pub struct PolicyPut {
    idle_threshold_secs: u64,
    locked_is_offline: bool,
    online_confirm_secs: u64,
}

#[derive(Serialize)]
struct StatusBody {
    online: bool,
    idle_secs: u64,
    locked: bool,
    since_ms: i64,
    today_online_secs: i64,
}

#[derive(Serialize)]
struct SegmentBody {
    id: i64,
    state: &'static str,
    started_at: i64,
    ended_at: Option<i64>,
    end_reason: Option<&'static str>,
}

#[derive(Serialize)]
struct ClientBody {
    id: String,
    name: String,
    scopes: Vec<Scope>,
    created_at: i64,
    revoked_at: Option<i64>,
}

#[derive(Deserialize)]
pub struct ClientCreate {
    name: String,
    scopes: Vec<Scope>,
}

#[derive(Deserialize)]
pub struct ClientPatch {
    name: Option<String>,
    scopes: Option<Vec<Scope>>,
}

#[derive(Deserialize)]
pub struct RangeQuery {
    from: Option<String>,
    to: Option<String>,
}

#[derive(Deserialize)]
pub struct QueryFilter {
    client_id: Option<String>,
    limit: Option<u32>,
}

pub async fn status(State(state): State<AppState>) -> Response {
    let live = state.live.get();
    let now = now_ms();
    let today = match state.db.segments_between(start_of_today_ms(), now) {
        Ok(segments) => overlap_online(&segments, start_of_today_ms(), now, now).0,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    Json(StatusBody {
        online: live.online,
        idle_secs: live.idle_secs,
        locked: live.locked,
        since_ms: live.since_ms,
        today_online_secs: today,
    })
    .into_response()
}

pub async fn get_policy(State(state): State<AppState>) -> Response {
    match state.db.policy() {
        Ok(policy) => Json(policy_body(policy)).into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn put_policy(State(state): State<AppState>, Json(body): Json<PolicyPut>) -> Response {
    match state.db.save_knobs(
        body.idle_threshold_secs,
        body.locked_is_offline,
        body.online_confirm_secs,
    ) {
        Ok(policy) => Json(policy_body(policy)).into_response(),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub async fn sessions(State(state): State<AppState>, Query(query): Query<RangeQuery>) -> Response {
    let (from_ms, to_ms) = match resolve_range(query.from.as_deref(), query.to.as_deref()) {
        Ok(range) => range,
        Err(err) => return fail(StatusCode::BAD_REQUEST, &err),
    };
    match state.db.segments_between(from_ms, to_ms) {
        Ok(segments) => Json(json!({
            "from": from_ms,
            "to": to_ms,
            "segments": segments.iter().map(segment_body).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn summary(State(state): State<AppState>, Query(query): Query<RangeQuery>) -> Response {
    let (from_ms, to_ms) = match resolve_range(query.from.as_deref(), query.to.as_deref()) {
        Ok(range) => range,
        Err(err) => return fail(StatusCode::BAD_REQUEST, &err),
    };
    let now = now_ms();
    match state.db.segments_between(from_ms, to_ms) {
        Ok(segments) => {
            let (online_secs, segment_count) = overlap_online(&segments, from_ms, to_ms, now);
            Json(json!({
                "from": from_ms,
                "to": to_ms,
                "online_secs": online_secs,
                "segment_count": segment_count,
            }))
            .into_response()
        }
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn list_clients(State(state): State<AppState>) -> Response {
    match state.db.list_clients() {
        Ok(clients) => Json(
            clients
                .into_iter()
                .map(client_body)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn create_client(
    State(state): State<AppState>,
    Json(body): Json<ClientCreate>,
) -> Response {
    let name = match validate_name(&body.name) {
        Ok(name) => name,
        Err(err) => return fail(StatusCode::BAD_REQUEST, &err),
    };
    let (token, hash) = match issue_token() {
        Ok(pair) => pair,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    let id = match new_id() {
        Ok(id) => id,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    match state
        .db
        .create_client(&id, &name, &hash, &body.scopes, now_ms())
    {
        Ok(client) => (
            StatusCode::CREATED,
            Json(json!({
                "client": client_body(client),
                "token": token,
            })),
        )
            .into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn patch_client(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ClientPatch>,
) -> Response {
    if body.name.is_none() && body.scopes.is_none() {
        return fail(StatusCode::BAD_REQUEST, "没有要修改的内容");
    }
    let name = match body.name.as_deref().map(validate_name).transpose() {
        Ok(name) => name,
        Err(err) => return fail(StatusCode::BAD_REQUEST, &err),
    };
    match state
        .db
        .update_client(&id, name.as_deref(), body.scopes.as_deref())
    {
        Ok(Some(client)) => Json(client_body(client)).into_response(),
        Ok(None) => fail(StatusCode::NOT_FOUND, "找不到这个客户端"),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub async fn revoke_client(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.db.revoke_client(&id, now_ms()) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => fail(StatusCode::NOT_FOUND, "找不到这个客户端，或它已经撤销"),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn queries(State(state): State<AppState>, Query(query): Query<QueryFilter>) -> Response {
    let client_id = query
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match state.db.list_queries(client_id, query.limit.unwrap_or(100)) {
        Ok(rows) => Json(json!({
            "queries": rows.iter().map(|row| json!({
                "id": row.id,
                "client_id": row.client_id,
                "client_name": row.client_name,
                "tool": row.tool,
                "allowed": row.allowed,
                "args_summary": row.args_summary,
                "result_summary": row.result_summary,
                "created_at": row.created_at,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

fn resolve_range(from: Option<&str>, to: Option<&str>) -> Result<(i64, i64), String> {
    let now = now_ms();
    let from_ms = match from.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => parse_range_bound(value, false)?,
        None => start_of_today_ms(),
    };
    let to_ms = match to.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => parse_range_bound(value, true)?,
        None => now,
    };
    if to_ms < from_ms {
        return Err("结束时间早于开始时间".into());
    }
    Ok((from_ms, to_ms))
}

fn policy_body(policy: Policy) -> PolicyBody {
    PolicyBody {
        idle_threshold_secs: policy.idle_threshold_secs,
        locked_is_offline: policy.locked_is_offline,
        online_confirm_secs: policy.online_confirm_secs,
    }
}

fn segment_body(segment: &Segment) -> SegmentBody {
    SegmentBody {
        id: segment.id,
        state: segment.state.as_str(),
        started_at: segment.started_at,
        ended_at: segment.ended_at,
        end_reason: segment.end_reason.map(|reason| reason.as_str()),
    }
}

fn client_body(client: Client) -> ClientBody {
    ClientBody {
        id: client.id,
        name: client.name,
        scopes: client.scopes,
        created_at: client.created_at,
        revoked_at: client.revoked_at,
    }
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
