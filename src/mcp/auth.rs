
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use getrandom::fill;
use sha2::{Digest, Sha256};
use tokio::task_local;

use crate::{db::Db, model::Scope};

#[derive(Clone, Debug)]
pub struct ClientAuth {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
}

task_local! {
    pub static CURRENT_CLIENT: ClientAuth;
}

pub fn current_client() -> Option<ClientAuth> {
    CURRENT_CLIENT.try_with(|client| client.clone()).ok()
}

pub async fn require_client(State(db): State<Db>, req: Request<Body>, next: Next) -> Response {
    if !req.uri().path().starts_with("/mcp") {
        return next.run(req).await;
    }
    let token = bearer(req.headers().get(header::AUTHORIZATION));
    let Some(token) = token else {
        return unauthorized();
    };
    let client = match db.client_by_token_hash(&token_hash(&token)) {
        Ok(Some(client)) if client.revoked_at.is_none() => ClientAuth {
            id: client.id,
            name: client.name,
            scopes: client.scopes,
        },
        Ok(_) => return unauthorized(),
        Err(err) => {
            tracing::error!(error = %err, "校验 MCP token 失败");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    CURRENT_CLIENT.scope(client, next.run(req)).await
}

pub fn issue_token() -> anyhow::Result<(String, String)> {
    let mut bytes = [0u8; 32];
    fill(&mut bytes)?;
    let token = format!("awk_{}", base64url(&bytes));
    let hash = token_hash(&token);
    Ok((token, hash))
}

pub fn new_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    fill(&mut bytes)?;
    Ok(hex(&bytes))
}

pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn bearer(value: Option<&axum::http::HeaderValue>) -> Option<String> {
    let raw = value?.to_str().ok()?;
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() { None } else { Some(token.to_string()) }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        "需要 Bearer token",
    )
        .into_response()
}

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(TABLE[((n >> 6) & 63) as usize] as char);
        out.push(TABLE[(n & 63) as usize] as char);
        i += 3;
    }
    if i < bytes.len() {
        let mut n = u32::from(bytes[i]) << 16;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        if i + 1 < bytes.len() {
            n |= u32::from(bytes[i + 1]) << 8;
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0xf) as usize] as char);
    }
    out
}

