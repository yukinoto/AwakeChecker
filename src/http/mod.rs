mod api;

use std::net::SocketAddr;

use axum::{
    Router,
    middleware,
    response::{Html, IntoResponse},
    routing::{get, patch, post},
};
use tokio::{net::TcpListener, sync::watch};

use crate::{db::Db, live::Live, mcp};

const INDEX: &str = include_str!("../../assets/index.html");
const APP_JS: &str = include_str!("../../assets/app.js");
const STYLE: &str = include_str!("../../assets/style.css");

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub live: Live,
}

pub async fn serve(
    addr: SocketAddr,
    state: AppState,
    shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            let message = format!("无法监听 {addr}：{err}");
            tracing::error!("{message}");
            crate::win::report_fatal(&message);
            return Err(err.into());
        }
    };
    serve_listener(listener, state, shutdown).await
}

pub async fn serve_listener(
    listener: TcpListener,
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let bound = listener.local_addr()?;
    tracing::info!("网页 http://{bound}");
    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                if shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await?;
    Ok(())
}

fn router(state: AppState) -> Router {
    let mcp_service = mcp::http_service(state.clone());
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(javascript))
        .route("/style.css", get(stylesheet))
        .route("/api/status", get(api::status))
        .route("/api/policy", get(api::get_policy).put(api::put_policy))
        .route("/api/sessions", get(api::sessions))
        .route("/api/summary", get(api::summary))
        .route(
            "/api/mcp/clients",
            get(api::list_clients).post(api::create_client),
        )
        .route("/api/mcp/clients/{id}", patch(api::patch_client))
        .route(
            "/api/mcp/clients/{id}/revoke",
            post(api::revoke_client),
        )
        .route("/api/mcp/queries", get(api::queries))
        .with_state(state.clone())
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(state.db, mcp::require_client))
}

async fn index() -> impl IntoResponse {
    Html(INDEX)
}

async fn javascript() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        sync::watch,
    };

    use super::*;
    use crate::{
        db::Db,
        live::Live,
        mcp::{issue_token, new_id},
        model::{Scope, now_ms},
    };

    async fn post_mcp(port: u16, token: &str, body: &str) -> String {
        let mut stream = None;
        for _ in 0..30 {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let mut stream = stream.expect("服务没有开始监听");
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn scoped_tools_are_filtered_and_queries_are_stored() {
        let path = std::env::temp_dir().join(format!(
            "awake-http-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let db = Db::open(&path).unwrap();
        let (summary_token, summary_hash) = issue_token().unwrap();
        let (full_token, full_hash) = issue_token().unwrap();
        db.create_client(
            &new_id().unwrap(),
            "summary-only",
            &summary_hash,
            &[Scope::Summary],
            now_ms(),
        )
        .unwrap();
        db.create_client(
            &new_id().unwrap(),
            "full",
            &full_hash,
            &[Scope::Status, Scope::History, Scope::Summary],
            now_ms(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_tx, rx) = watch::channel(false);
        let state = AppState {
            db: db.clone(),
            live: Live::default(),
        };
        tokio::spawn(async move {
            let _ = serve_listener(listener, state, rx).await;
        });

        let summary_list = post_mcp(
            port,
            &summary_token,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )
        .await;
        assert!(
            summary_list.contains("summarize_presence"),
            "{summary_list}"
        );
        assert!(!summary_list.contains("list_sessions"), "{summary_list}");

        let denied = post_mcp(
            port,
            &summary_token,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}}"#,
        )
        .await;
        assert!(denied.contains("permission denied"), "{denied}");

        let status = post_mcp(
            port,
            &full_token,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_status","arguments":{}}}"#,
        )
        .await;
        assert!(status.contains("online"), "{status}");

        let queries = db.list_queries(None, 10).unwrap();
        assert!(queries.iter().any(|row| !row.allowed && row.tool == "list_sessions"));
        assert!(queries.iter().any(|row| row.allowed && row.tool == "get_status"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
