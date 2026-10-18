mod api;

use std::net::SocketAddr;

use axum::{
    Router, middleware,
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
        .route("/api/mcp/clients/{id}/revoke", post(api::revoke_client))
        .route("/api/mcp/queries", get(api::queries))
        .with_state(state.clone())
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(
            state.db,
            mcp::require_client,
        ))
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

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::watch,
    };
    use tower::ServiceExt;

    use super::*;
    use crate::{
        db::Db,
        live::Live,
        mcp::{issue_token, new_id},
        model::{EndReason, Presence, Scope, now_ms, start_of_today_ms},
    };

    fn temp_db() -> (Db, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("awake-api-{}-{n}.db", std::process::id()));
        (Db::open(&path).unwrap(), path)
    }

    async fn send(
        state: &AppState,
        method: &str,
        uri: &str,
        authorization: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, Value, axum::http::HeaderMap) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1");
        if let Some(value) = authorization {
            builder = builder.header("authorization", value);
        }
        if body.is_some() {
            builder = builder
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream");
        }
        let request = builder
            .body(match body {
                Some(text) => Body::from(text.to_owned()),
                None => Body::empty(),
            })
            .unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            router(state.clone()).oneshot(request),
        )
        .await
        .expect("请求超时")
        .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, value, headers)
    }

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
        let path =
            std::env::temp_dir().join(format!("awake-http-{}-{}.db", std::process::id(), now_ms()));
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
        assert!(
            queries
                .iter()
                .any(|row| !row.allowed && row.tool == "list_sessions")
        );
        assert!(
            queries
                .iter()
                .any(|row| row.allowed && row.tool == "get_status")
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[tokio::test]
    async fn policy_rejects_out_of_range_and_saves_valid_knobs() {
        let (db, path) = temp_db();
        let state = AppState {
            db: db.clone(),
            live: Live::default(),
        };
        let (status, body, _) = send(&state, "GET", "/api/policy", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["idle_threshold_secs"], json!(180));
        assert_eq!(body["locked_is_offline"], json!(true));
        assert_eq!(body["online_confirm_secs"], json!(5));

        let (status, body, _) = send(
            &state,
            "PUT",
            "/api/policy",
            None,
            Some(r#"{"idle_threshold_secs":9,"locked_is_offline":true,"online_confirm_secs":0}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("空闲"));
        assert_eq!(db.policy().unwrap().idle_threshold_secs, 180);

        let (status, body, _) = send(
            &state,
            "PUT",
            "/api/policy",
            None,
            Some(r#"{"idle_threshold_secs":30,"locked_is_offline":false,"online_confirm_secs":0}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["idle_threshold_secs"], json!(30));
        assert_eq!(body["locked_is_offline"], json!(false));
        assert_eq!(db.policy().unwrap().sample_interval_secs, 2);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn summary_counts_overlap_and_inverted_range_is_rejected() {
        let (db, path) = temp_db();
        db.insert_segment(Presence::Online, 0).unwrap();
        db.transition(Presence::Offline, 5_000, EndReason::Idle)
            .unwrap();
        let state = AppState {
            db,
            live: Live::default(),
        };
        let (status, body, _) = send(
            &state,
            "GET",
            "/api/sessions?from=2026-09-29T00:00:02Z&to=2026-09-29T00:00:01Z",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("结束时间"));

        let (status, body, _) = send(
            &state,
            "GET",
            "/api/summary?from=1970-01-01T00:00:00Z&to=1970-01-01T00:00:10Z",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["online_secs"], json!(5));
        assert_eq!(body["segment_count"], json!(1));

        let (status, body, _) = send(
            &state,
            "GET",
            "/api/sessions?from=1970-01-01T00:00:00Z&to=1970-01-01T00:00:10Z",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["segments"][0]["state"], json!("online"));
        assert_eq!(body["segments"][0]["end_reason"], json!("idle"));
        assert_eq!(body["segments"][1]["state"], json!("offline"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn status_reports_the_live_snapshot_and_today() {
        let (db, path) = temp_db();
        let start = start_of_today_ms();
        db.insert_segment(Presence::Online, start).unwrap();
        let live = Live::default();
        live.publish(crate::live::LiveSnapshot {
            online: true,
            idle_secs: 4,
            locked: false,
            since_ms: start,
        });
        let state = AppState { db, live };
        let before = now_ms();
        let (status, body, _) = send(&state, "GET", "/api/status", None, None).await;
        let after = now_ms();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["online"], json!(true));
        assert_eq!(body["idle_secs"], json!(4));
        assert_eq!(body["locked"], json!(false));
        assert_eq!(body["since_ms"], json!(start));
        let today = body["today_online_secs"].as_i64().unwrap();
        assert!(today >= (before - start) / 1000);
        assert!(today <= (after - start) / 1000 + 1);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn client_token_is_shown_once_and_revoke_sticks() {
        let (db, path) = temp_db();
        let state = AppState {
            db: db.clone(),
            live: Live::default(),
        };
        let (status, body, _) = send(
            &state,
            "POST",
            "/api/mcp/clients",
            None,
            Some(r#"{"name":"  ","scopes":["status"]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("名称"));

        let (status, body, _) = send(
            &state,
            "POST",
            "/api/mcp/clients",
            None,
            Some(r#"{"name":"  座位  ","scopes":["status","summary"]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let token = body["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("awk_"));
        let id = body["client"]["id"].as_str().unwrap().to_string();
        assert_eq!(body["client"]["name"], json!("座位"));

        let (status, listed, _) = send(&state, "GET", "/api/mcp/clients", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!listed.to_string().contains(&token));

        let (status, _, _) = send(
            &state,
            "PATCH",
            &format!("/api/mcp/clients/{id}"),
            None,
            Some("{}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, body, _) = send(
            &state,
            "PATCH",
            &format!("/api/mcp/clients/{id}"),
            None,
            Some(r#"{"name":"改名","scopes":["history"]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["name"], json!("改名"));
        assert_eq!(body["scopes"], json!(["history"]));

        let (status, _, _) = send(
            &state,
            "POST",
            &format!("/api/mcp/clients/{id}/revoke"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body, _) = send(
            &state,
            "PATCH",
            &format!("/api/mcp/clients/{id}"),
            None,
            Some(r#"{"name":"again"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["error"].as_str().unwrap().contains("撤销"));
        let (status, body, _) = send(
            &state,
            "POST",
            &format!("/api/mcp/clients/{id}/revoke"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("撤销"));
        let (status, _, _) = send(
            &state,
            "PATCH",
            "/api/mcp/clients/missing",
            None,
            Some(r#"{"name":"x"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn queries_can_be_filtered_by_client() {
        let (db, path) = temp_db();
        db.insert_query("a", "get_status", true, "", "online=true", 1)
            .unwrap();
        db.insert_query("b", "list_sessions", false, "{}", "permission denied", 2)
            .unwrap();
        let state = AppState {
            db,
            live: Live::default(),
        };
        let (status, body, _) = send(
            &state,
            "GET",
            "/api/mcp/queries?client_id=b&limit=10",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["queries"].as_array().unwrap().len(), 1);
        assert_eq!(body["queries"][0]["tool"], json!("list_sessions"));
        assert_eq!(body["queries"][0]["allowed"], json!(false));
        let (status, body, _) = send(
            &state,
            "GET",
            "/api/mcp/queries?client_id=%20%20",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["queries"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn mcp_requires_a_live_bearer_token() {
        let (db, path) = temp_db();
        let (token, hash) = issue_token().unwrap();
        let id = new_id().unwrap();
        db.create_client(&id, "agent", &hash, &[Scope::Summary], now_ms())
            .unwrap();
        let state = AppState {
            db: db.clone(),
            live: Live::default(),
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let (status, body, headers) = send(&state, "POST", "/mcp", None, Some(list)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(headers.get("www-authenticate").is_some());
        assert!(body.as_str().unwrap_or("").contains("Bearer"));

        let (status, _, _) = send(&state, "POST", "/mcp", Some("Bearer "), Some(list)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = send(
            &state,
            "POST",
            "/mcp",
            Some("Bearer awk_missing"),
            Some(list),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body, _) = send(
            &state,
            "POST",
            "/mcp",
            Some(&format!("bearer {token}")),
            Some(list),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let text = body.to_string();
        assert!(text.contains("summarize_presence"), "{text}");
        assert!(!text.contains("list_sessions"), "{text}");

        assert!(db.revoke_client(&id, now_ms()).unwrap());
        let (status, _, _) = send(
            &state,
            "POST",
            "/mcp",
            Some(&format!("Bearer {token}")),
            Some(list),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn static_assets_are_served() {
        let (db, path) = temp_db();
        let state = AppState {
            db,
            live: Live::default(),
        };
        let (status, body, _) = send(&state, "GET", "/", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap().contains("AwakeChecker"));
        let (status, _, headers) = send(&state, "GET", "/app.js", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .contains("javascript")
        );
        let (status, _, headers) = send(&state, "GET", "/style.css", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers["content-type"]
                .to_str()
                .unwrap()
                .contains("text/css")
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    #[cfg(not(windows))]
    async fn serve_reports_when_the_port_is_taken() {
        let (db, path) = temp_db();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_tx, rx) = watch::channel(false);
        let err = serve(
            addr,
            AppState {
                db,
                live: Live::default(),
            },
            rx,
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().is_empty());
        drop(listener);
        let _ = std::fs::remove_file(path);
    }
}
