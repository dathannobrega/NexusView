//! MCP server core (RF-08).
//!
//! Implements the Model Context Protocol over JSON-RPC 2.0: `initialize`,
//! `tools/list`, and `tools/call`, plus a set of tools that expose an open
//! NexusView timeline so a local LLM agent can search, page, and summarize it in
//! real time. The transport (newline-delimited JSON over stdio) lives in
//! `main.rs`; this module is pure and unit-tested via [`Server::handle_line`].

use std::collections::HashMap;

use nexus_core::{Dataset, ParserSchema, View};
use serde_json::{json, Map, Value};

/// MCP protocol version advertised by this server.
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "nexus-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// JSON-RPC error codes used by the server.
mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

/// One opened timeline plus its current filtered view.
struct Timeline {
    path: String,
    dataset: Dataset,
    /// The active view (set by `search`; defaults to all rows).
    current_view: View,
}

/// The MCP server state: a registry of open timelines. Requests are processed
/// sequentially, so no locking is needed.
pub struct Server {
    timelines: HashMap<u64, Timeline>,
    next_id: u64,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server {
            timelines: HashMap::new(),
            next_id: 1,
        }
    }

    /// Handle one JSON-RPC message line. Returns the response line to write, or
    /// `None` for notifications (which get no reply).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return Some(error_response(
                    Value::Null,
                    codes::PARSE_ERROR,
                    "parse error",
                ))
            }
        };

        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str);
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        let Some(method) = method else {
            // No method → only valid as nothing; if it had an id, it's invalid.
            return id.map(|id| error_response(id, codes::INVALID_REQUEST, "missing method"));
        };

        // Notifications (no id) are handled without a response.
        let id = id?;

        match self.dispatch(method, params) {
            Ok(result) => Some(success_response(id, result)),
            Err(err) => Some(error_response(id, err.code, &err.message)),
        }
    }

    fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => self.handle_tool_call(params),
            other => Err(RpcError::new(
                codes::METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            )),
        }
    }

    /// `tools/call` wraps tool output in MCP content; tool-level failures are
    /// reported as `isError` content, not protocol errors.
    fn handle_tool_call(&mut self, params: Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(codes::INVALID_PARAMS, "missing tool name".into()))?
            .to_string();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        let (text, is_error) = match self.call_tool(&name, &args) {
            Ok(value) => (
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into()),
                false,
            ),
            Err(message) => (message, true),
        };

        Ok(json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": is_error,
        }))
    }

    /// Dispatch a tool by name. Returns the result data or a human-readable error.
    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "open_timeline" => self.open_timeline(args),
            "list_timelines" => Ok(self.list_timelines()),
            "timeline_info" => self.timeline_info(args),
            "search" => self.search(args),
            "reset_filter" => self.reset_filter(args),
            "get_rows" => self.get_rows(args),
            "column_distribution" => self.column_distribution(args),
            "close_timeline" => self.close_timeline(args),
            other => Err(format!("unknown tool: {other}")),
        }
    }

    // --- Tools ------------------------------------------------------------

    fn open_timeline(&mut self, args: &Value) -> Result<Value, String> {
        let path = arg_str(args, "path")?;
        let schema = match args.get("schema_json").and_then(Value::as_str) {
            Some(doc) if !doc.trim().is_empty() => {
                Some(ParserSchema::from_str_auto(doc).map_err(|e| e.to_string())?)
            }
            _ => None,
        };

        let dataset = Dataset::open(path, schema).map_err(|e| e.to_string())?;
        let columns: Vec<String> = dataset.columns().to_vec();
        let row_count = dataset.row_count();
        let current_view = dataset.view_all();

        let id = self.next_id;
        self.next_id += 1;
        self.timelines.insert(
            id,
            Timeline {
                path: path.to_string(),
                dataset,
                current_view,
            },
        );

        Ok(json!({ "timeline_id": id, "columns": columns, "row_count": row_count }))
    }

    fn list_timelines(&self) -> Value {
        let mut list: Vec<Value> = self
            .timelines
            .iter()
            .map(|(id, tl)| {
                json!({
                    "timeline_id": id,
                    "path": tl.path,
                    "row_count": tl.dataset.row_count(),
                    "view_count": tl.current_view.len(),
                    "columns": tl.dataset.columns(),
                })
            })
            .collect();
        list.sort_by_key(|v| v.get("timeline_id").and_then(Value::as_u64).unwrap_or(0));
        json!({ "timelines": list })
    }

    fn timeline_info(&self, args: &Value) -> Result<Value, String> {
        let tl = self.timeline(args)?;
        Ok(json!({
            "path": tl.path,
            "columns": tl.dataset.columns(),
            "row_count": tl.dataset.row_count(),
            "view_count": tl.current_view.len(),
        }))
    }

    fn search(&mut self, args: &Value) -> Result<Value, String> {
        let id = arg_timeline_id(args)?;
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        let tl = self
            .timelines
            .get_mut(&id)
            .ok_or_else(|| format!("no timeline {id}"))?;
        let view = tl.dataset.search_view(query).map_err(|e| e.to_string())?;
        let matched = view.len();
        tl.current_view = view;
        Ok(json!({ "matched": matched, "total": tl.dataset.row_count() }))
    }

    fn reset_filter(&mut self, args: &Value) -> Result<Value, String> {
        let id = arg_timeline_id(args)?;
        let tl = self
            .timelines
            .get_mut(&id)
            .ok_or_else(|| format!("no timeline {id}"))?;
        tl.current_view = tl.dataset.view_all();
        Ok(json!({ "view_count": tl.current_view.len() }))
    }

    fn get_rows(&self, args: &Value) -> Result<Value, String> {
        let tl = self.timeline(args)?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .min(1000);

        let columns = tl.dataset.columns();
        let count = tl.current_view.len();
        let mut rows = Vec::new();
        let end = offset.saturating_add(limit).min(count);
        let mut i = offset;
        while i < end {
            if let Some(rid) = tl.current_view.row_id(i) {
                let values = tl.dataset.row_values(rid as usize);
                let mut object = Map::new();
                for (col, value) in columns.iter().zip(values) {
                    object.insert(col.clone(), Value::String(value));
                }
                rows.push(Value::Object(object));
            }
            i += 1;
        }

        Ok(json!({
            "offset": offset,
            "limit": limit,
            "view_count": count,
            "rows": rows,
        }))
    }

    fn column_distribution(&self, args: &Value) -> Result<Value, String> {
        let tl = self.timeline(args)?;
        let column = resolve_column(&tl.dataset, args.get("column"))?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;

        let tree = tl.dataset.group(&tl.current_view, &[column]);
        let mut items: Vec<(String, u64)> = (0..tree.root_count())
            .filter_map(|i| {
                let item = tree.root_child(i)?;
                Some((tree.label(item).to_string(), tree.count(item)))
            })
            .collect();
        let distinct = items.len();
        items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        items.truncate(limit);

        let values: Vec<Value> = items
            .into_iter()
            .map(|(value, count)| json!({ "value": value, "count": count }))
            .collect();

        Ok(json!({
            "column": tl.dataset.column_name(column).unwrap_or(""),
            "distinct_total": distinct,
            "values": values,
        }))
    }

    fn close_timeline(&mut self, args: &Value) -> Result<Value, String> {
        let id = arg_timeline_id(args)?;
        let removed = self.timelines.remove(&id).is_some();
        Ok(json!({ "closed": removed }))
    }

    // --- Helpers ----------------------------------------------------------

    fn timeline(&self, args: &Value) -> Result<&Timeline, String> {
        let id = arg_timeline_id(args)?;
        self.timelines
            .get(&id)
            .ok_or_else(|| format!("no timeline {id}"))
    }
}

/// Resolve a column reference (name, case-insensitive, or numeric index).
fn resolve_column(dataset: &Dataset, value: Option<&Value>) -> Result<usize, String> {
    match value {
        Some(Value::Number(n)) => {
            let idx =
                n.as_u64()
                    .ok_or("column index must be a non-negative integer")? as usize;
            if idx < dataset.column_count() {
                Ok(idx)
            } else {
                Err(format!("column index {idx} out of range"))
            }
        }
        Some(Value::String(name)) => dataset
            .columns()
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no column named '{name}'")),
        _ => Err("'column' must be a name or index".into()),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing or non-string argument '{key}'"))
}

fn arg_timeline_id(args: &Value) -> Result<u64, String> {
    args.get("timeline_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing 'timeline_id'".to_string())
}

/// The advertised tool catalog with JSON-Schema input definitions.
fn tool_definitions() -> Vec<Value> {
    let tl_id =
        json!({ "timeline_id": { "type": "integer", "description": "id from open_timeline" } });
    vec![
        json!({
            "name": "open_timeline",
            "description": "Open and index a delimited evidence file; returns a timeline_id, columns, and row_count.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "absolute path to the file" },
                    "schema_json": { "type": "string", "description": "optional JSON/YAML parser schema" }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "list_timelines",
            "description": "List all currently open timelines.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "timeline_info",
            "description": "Columns, total rows, and current filtered view size for a timeline.",
            "inputSchema": { "type": "object", "properties": tl_id, "required": ["timeline_id"] }
        }),
        json!({
            "name": "search",
            "description": "Filter a timeline with a query (boolean AND/OR/NOT, /regex/, col:term). Sets the current view. Empty query selects all.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeline_id": { "type": "integer" },
                    "query": { "type": "string", "description": "e.g. \"error AND NOT timeout\", host:web01, /c2_\\w+/" }
                },
                "required": ["timeline_id", "query"]
            }
        }),
        json!({
            "name": "reset_filter",
            "description": "Clear the current filter so the view is all rows again.",
            "inputSchema": { "type": "object", "properties": tl_id, "required": ["timeline_id"] }
        }),
        json!({
            "name": "get_rows",
            "description": "Page rows of the current view as objects keyed by column name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeline_id": { "type": "integer" },
                    "offset": { "type": "integer", "default": 0 },
                    "limit": { "type": "integer", "default": 50, "maximum": 1000 }
                },
                "required": ["timeline_id"]
            }
        }),
        json!({
            "name": "column_distribution",
            "description": "Value counts for a column over the current view, most frequent first (great for triage summaries).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "timeline_id": { "type": "integer" },
                    "column": { "description": "column name or index" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["timeline_id", "column"]
            }
        }),
        json!({
            "name": "close_timeline",
            "description": "Close a timeline and release its memory.",
            "inputSchema": { "type": "object", "properties": tl_id, "required": ["timeline_id"] }
        }),
    ]
}

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: String) -> Self {
        RpcError { code, message }
    }
}

fn success_response(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_csv(contents: &str) -> tempfile::NamedTempFile {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(contents.as_bytes()).unwrap();
        tf.flush().unwrap();
        tf
    }

    /// Parse a `tools/call` response and return the inner data value, asserting
    /// it was not an error.
    fn tool_data(response: &str) -> Value {
        let v: Value = serde_json::from_str(response).unwrap();
        let result = &v["result"];
        assert_eq!(
            result["isError"],
            json!(false),
            "tool returned error: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    fn call(server: &mut Server, id: u64, name: &str, args: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        let resp = server.handle_line(&req.to_string()).unwrap();
        tool_data(&resp)
    }

    #[test]
    fn initialize_reports_server_info() {
        let mut s = Server::new();
        let resp = s
            .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["serverInfo"]["name"], "nexus-mcp");
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn notifications_get_no_response() {
        let mut s = Server::new();
        assert!(s
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn tools_list_includes_search() {
        let mut s = Server::new();
        let resp = s
            .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"search"));
        assert!(names.contains(&"open_timeline"));
        assert!(names.contains(&"column_distribution"));
    }

    #[test]
    fn unknown_method_is_an_error() {
        let mut s = Server::new();
        let resp = s
            .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#)
            .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn full_agent_workflow() {
        let file =
            temp_csv("host,sev,msg\nweb01,INFO,ok\nweb02,ERROR,disk full\nweb01,ERROR,timeout\n");
        let mut s = Server::new();

        let opened = call(
            &mut s,
            1,
            "open_timeline",
            json!({ "path": file.path().to_str().unwrap() }),
        );
        let id = opened["timeline_id"].as_u64().unwrap();
        assert_eq!(opened["row_count"], 3);
        assert_eq!(opened["columns"], json!(["host", "sev", "msg"]));

        // Search filters to the 2 ERROR rows.
        let searched = call(
            &mut s,
            2,
            "search",
            json!({ "timeline_id": id, "query": "sev:ERROR" }),
        );
        assert_eq!(searched["matched"], 2);

        // get_rows pages the current (filtered) view.
        let rows = call(
            &mut s,
            3,
            "get_rows",
            json!({ "timeline_id": id, "offset": 0, "limit": 10 }),
        );
        assert_eq!(rows["view_count"], 2);
        assert_eq!(rows["rows"][0]["sev"], "ERROR");

        // Distribution over the filtered view (by host): web01=1, web02=1.
        let dist = call(
            &mut s,
            4,
            "column_distribution",
            json!({ "timeline_id": id, "column": "host" }),
        );
        assert_eq!(dist["distinct_total"], 2);

        // reset_filter restores all rows.
        let reset = call(&mut s, 5, "reset_filter", json!({ "timeline_id": id }));
        assert_eq!(reset["view_count"], 3);

        let closed = call(&mut s, 6, "close_timeline", json!({ "timeline_id": id }));
        assert_eq!(closed["closed"], true);
    }

    #[test]
    fn tool_error_is_reported_as_iserror() {
        let mut s = Server::new();
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "open_timeline", "arguments": { "path": "/no/such/file.csv" } }
        });
        let resp = s.handle_line(&req.to_string()).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], json!(true));
    }
}
