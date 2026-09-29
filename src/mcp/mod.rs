mod access;
mod auth;
mod server;

use std::sync::Arc;

use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};

use crate::http::AppState;

pub use auth::{issue_token, new_id, require_client};

pub fn http_service(app: AppState) -> StreamableHttpService<server::AwakeServer, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    StreamableHttpService::new(
        move || {
            let client = auth::current_client()
                .ok_or_else(|| std::io::Error::other("缺少已认证的 MCP 客户端"))?;
            Ok(server::AwakeServer::new(app.clone(), client))
        },
        Arc::new(LocalSessionManager::default()),
        config,
    )
}
