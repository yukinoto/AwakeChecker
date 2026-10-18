use std::{borrow::Cow, sync::Arc};

use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
};
use serde_json::{Map, Value, json};

use crate::{
    http::AppState,
    mcp::{
        access::{
            TOOL_SESSIONS, TOOL_STATUS, TOOL_SUMMARY, args_summary, project_sessions,
            project_status, project_summary, result_summary, tool_allowed,
        },
        auth::ClientAuth,
    },
    model::{clip_text, default_range, now_ms, parse_range_bound},
};

#[derive(Clone)]
pub struct AwakeServer {
    app: AppState,
    client: ClientAuth,
}

impl AwakeServer {
    pub fn new(app: AppState, client: ClientAuth) -> Self {
        Self { app, client }
    }

    fn tools(&self) -> Vec<Tool> {
        let mut tools = Vec::new();
        if tool_allowed(&self.client.scopes, TOOL_STATUS) {
            tools.push(read_only_tool(
                TOOL_STATUS,
                "查看当前是否在电脑前。返回里的 visible 列出这个客户端实际能看到的字段；没出现的字段是权限限制，不是没有数据。",
                empty_schema(),
            ));
        }
        if tool_allowed(&self.client.scopes, TOOL_SESSIONS) {
            tools.push(read_only_tool(
                TOOL_SESSIONS,
                "列出一段时间里的在线和离开时段。from 和 to 可以是 RFC3339，或 YYYY-MM-DD（按这台电脑的本地日期）。省略时从今天零点到现在。",
                range_schema(true),
            ));
        }
        if tool_allowed(&self.client.scopes, TOOL_SUMMARY) {
            tools.push(read_only_tool(
                TOOL_SUMMARY,
                "汇总一段时间的在线总秒数和在线段数，不返回每一段的起止时间。from 和 to 可以是 RFC3339 或 YYYY-MM-DD。省略时统计今天到现在。",
                range_schema(false),
            ));
        }
        tools
    }

    fn audit(&self, tool: &str, allowed: bool, args: &str, result: &str) -> Result<(), String> {
        self.app
            .db
            .insert_query(&self.client.id, tool, allowed, args, result, now_ms())
            .map_err(|err| {
                tracing::error!(error = %err, "写入 MCP 查询记录失败");
                "查询记录写入失败".to_string()
            })
    }
}

impl ServerHandler for AwakeServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("awake-checker", env!("CARGO_PKG_VERSION"))
                    .with_title("AwakeChecker"),
            )
            .with_instructions(
                "查询这台电脑前是否有人。每个客户端只能看到被授予的信息：是否在线、当前段开始时间、空闲与锁屏、时段明细、在线总时长。",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let tool = request.name.to_string();
        tracing::info!(client = %self.client.name, %tool, "MCP 调用");
        let args_text = args_summary(request.arguments.as_ref());
        let arguments = request.arguments.unwrap_or_default();
        let outcome = match tool.as_str() {
            TOOL_STATUS => self.call_status(),
            TOOL_SESSIONS => self.call_sessions(&arguments),
            TOOL_SUMMARY => self.call_summary(&arguments),
            _ => Err(denied(&format!("unknown tool: {tool}"))),
        };
        match outcome {
            Ok(value) => {
                let summary = result_summary(&value);
                if let Err(err) = self.audit(&tool, true, &args_text, &summary) {
                    return Ok(tool_error(&err));
                }
                Ok(CallToolResult::structured(value).into())
            }
            Err(message) => {
                let summary = clip_text(&message, 180);
                if let Err(err) = self.audit(&tool, false, &args_text, &summary) {
                    return Ok(tool_error(&err));
                }
                Ok(tool_error(&message))
            }
        }
    }
}

impl AwakeServer {
    fn call_status(&self) -> Result<Value, String> {
        if !tool_allowed(&self.client.scopes, TOOL_STATUS) {
            return Err("permission denied: status".into());
        }
        project_status(&self.app.live.get(), &self.client.scopes)
            .map_err(|_| "permission denied: status".to_string())
    }

    fn call_sessions(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        if !tool_allowed(&self.client.scopes, TOOL_SESSIONS) {
            return Err("permission denied: history".into());
        }
        let (from_ms, to_ms) = resolve_range(arguments)?;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 1000) as usize;
        let segments = self
            .app
            .db
            .segments_between(from_ms, to_ms)
            .map_err(|_| "读取时段失败".to_string())?;
        Ok(project_sessions(&segments, limit))
    }

    fn call_summary(&self, arguments: &Map<String, Value>) -> Result<Value, String> {
        if !tool_allowed(&self.client.scopes, TOOL_SUMMARY) {
            return Err("permission denied: summary".into());
        }
        let (from_ms, to_ms) = resolve_range(arguments)?;
        let segments = self
            .app
            .db
            .segments_between(from_ms, to_ms)
            .map_err(|_| "读取时段失败".to_string())?;
        Ok(project_summary(&segments, from_ms, to_ms, now_ms()))
    }
}

fn resolve_range(arguments: &Map<String, Value>) -> Result<(i64, i64), String> {
    let now = now_ms();
    let (default_from, _) = default_range(now);
    let from_ms = match optional_str(arguments, "from") {
        Some(value) => parse_range_bound(value, false)?,
        None => default_from,
    };
    let to_ms = match optional_str(arguments, "to") {
        Some(value) => parse_range_bound(value, true)?,
        None => now,
    };
    if to_ms < from_ms {
        return Err("结束时间早于开始时间".into());
    }
    Ok((from_ms, to_ms))
}

fn optional_str<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn denied(message: &str) -> String {
    message.to_string()
}

fn tool_error(message: &str) -> CallToolResponse {
    CallToolResult::structured_error(json!({ "error": message })).into()
}

fn read_only_tool(
    name: &'static str,
    description: &'static str,
    schema: Arc<rmcp::model::JsonObject>,
) -> Tool {
    Tool::new(Cow::Borrowed(name), Cow::Borrowed(description), schema).annotate(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .open_world(false),
    )
}

fn empty_schema() -> Arc<rmcp::model::JsonObject> {
    schema(json!({}))
}

fn range_schema(with_limit: bool) -> Arc<rmcp::model::JsonObject> {
    let mut properties = json!({
        "from": { "type": "string", "description": "开始时间，RFC3339 或 YYYY-MM-DD。省略则从今天零点起。" },
        "to": { "type": "string", "description": "结束时间，RFC3339 或 YYYY-MM-DD。省略则到现在。" }
    });
    if with_limit {
        properties["limit"] = json!({
            "type": "integer",
            "description": "最多返回多少段，默认 100，最大 1000。"
        });
    }
    schema(properties)
}

fn schema(properties: Value) -> Arc<rmcp::model::JsonObject> {
    let value = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false
    });
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::{
        db::Db,
        http::AppState,
        live::{Live, LiveSnapshot},
        mcp::auth::ClientAuth,
        model::{EndReason, Presence, Scope},
    };

    fn harness(scopes: &[Scope]) -> (AwakeServer, Db, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("awake-mcp-{}-{n}.db", std::process::id()));
        let db = Db::open(&path).unwrap();
        let server = AwakeServer::new(
            AppState {
                db: db.clone(),
                live: Live::default(),
            },
            ClientAuth {
                id: "c1".into(),
                name: "agent".into(),
                scopes: scopes.to_vec(),
            },
        );
        (server, db, path)
    }

    #[test]
    fn tools_follow_scopes_and_are_marked_read_only() {
        let (server, _, empty_path) = harness(&[Scope::Since, Scope::Idle]);
        assert!(server.tools().is_empty());
        drop(server);
        let _ = std::fs::remove_file(empty_path);
        let (server, _, path) = harness(&[Scope::Status, Scope::History, Scope::Summary]);
        let names: Vec<_> = server
            .tools()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "get_status".to_string(),
                "list_sessions".to_string(),
                "summarize_presence".to_string()
            ]
        );
        let annotations = server.tools()[0].annotations.clone().unwrap();
        assert_eq!(annotations.read_only_hint, Some(true));
        assert_eq!(annotations.destructive_hint, Some(false));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn calls_honor_scope_and_clip_session_lists() {
        let (server, db, path) = harness(&[Scope::Status, Scope::Since, Scope::Idle]);
        server.app.live.publish(LiveSnapshot {
            online: true,
            idle_secs: 3,
            locked: true,
            since_ms: 1_700_000_000_000,
        });
        let status = server.call_status().unwrap();
        assert_eq!(status["online"], json!(true));
        assert_eq!(status["idle_secs"], json!(3));
        assert!(status["since"].as_str().unwrap().contains('T'));
        assert!(server.call_sessions(&Map::new()).is_err());
        assert!(server.call_summary(&Map::new()).is_err());

        db.insert_segment(Presence::Online, 0).unwrap();
        db.transition(Presence::Offline, 5_000, EndReason::Idle)
            .unwrap();
        let history = AwakeServer::new(
            server.app.clone(),
            ClientAuth {
                id: "c1".into(),
                name: "agent".into(),
                scopes: vec![Scope::History, Scope::Summary],
            },
        );
        assert!(history.call_status().is_err());
        let mut arguments = Map::new();
        arguments.insert("from".into(), json!("1970-01-01T00:00:00Z"));
        arguments.insert("to".into(), json!("1970-01-01T00:00:10Z"));
        arguments.insert("limit".into(), json!(0));
        let sessions = history.call_sessions(&arguments).unwrap();
        assert_eq!(sessions["count"], json!(1));
        let summary = history.call_summary(&arguments).unwrap();
        assert_eq!(summary["online_secs"], json!(5));
        assert!(summary.get("sessions").is_none());

        arguments.insert("from".into(), json!("2026-02-02T00:00:02Z"));
        arguments.insert("to".into(), json!("2026-02-02T00:00:01Z"));
        assert!(
            history
                .call_sessions(&arguments)
                .unwrap_err()
                .contains("结束时间")
        );
        arguments.insert("from".into(), json!("not-a-date"));
        assert!(history.call_summary(&arguments).is_err());
        arguments.clear();
        arguments.insert("from".into(), json!("  "));
        arguments.insert("to".into(), json!(""));
        assert!(history.call_summary(&arguments).is_ok());
        let _ = std::fs::remove_file(path);
    }
}
