//! [`HostClient`] — the plugin side of the host IPC channel
//! (`PLUGIN_SYSTEM.md` §6.6).
//!
//! When `clove <plugin>` runs a plugin, the host stays alive and serves the
//! clove tool surface over a local socket (`$CLOVE_HOST_SOCKET`, authenticated
//! with `$CLOVE_HOST_TOKEN`). A plugin reads and writes items through it with
//! the same arguments and results as the MCP tools ([`clove_types::tool`]), and
//! every write goes through clove's unified write path in the host. A plugin
//! that uses only this channel and the SDK crates never links an AGPL crate.
//!
//! ```no_run
//! # fn demo(cx: &clove_plugin::PluginContext) -> Result<(), clove_plugin::HostError> {
//! use clove_plugin::tool::{IdArgs, NewArgs};
//! let mut host = cx.host()?;
//! let created = host.create(&NewArgs {
//!     title: "Imported task".into(),
//!     item_type: None,
//!     priority: Some(1),
//!     labels: None,
//!     deps: None,
//!     parent: None,
//!     assignee: None,
//!     body: None,
//! })?;
//! # Ok(()) }
//! ```
//!
//! The client is synchronous and blocking; one connection answers one request
//! at a time, in order. Open a second client for concurrent calls.

use std::io::{BufRead, BufReader, Write};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, Stream};
use serde::Serialize;
use serde_json::{json, Value};

use clove_types::tool::*;
use clove_types::CloveError;

/// The host IPC protocol version this client speaks.
pub const HOST_API: u32 = 1;

/// Why a host call failed.
#[derive(Debug)]
pub enum HostError {
    /// No host channel: the plugin was not started by `clove`, or by a `clove`
    /// older than 0.2.0 (`$CLOVE_HOST_SOCKET` is not set).
    Unavailable,
    /// The socket could not be reached.
    Connect(std::io::Error),
    /// The host closed the connection (it exited, or rejected a request).
    HostGone,
    /// The host answered something this client cannot read, or a `hello` that
    /// does not match this plugin's repository.
    Protocol(String),
    /// The host refused the request (bad token, unknown method, bad params).
    Rpc { code: i64, message: String },
    /// The tool ran and failed. `clove_code`/`exit` are the code and exit status
    /// a built-in command reports for the same failure (`ITEM_NOT_FOUND` / 2, …).
    Tool {
        clove_code: String,
        exit: u8,
        message: String,
    },
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Unavailable => {
                f.write_str("no clove host channel: run this plugin through `clove` 0.2.0 or newer")
            }
            HostError::Connect(e) => write!(f, "cannot reach the clove host: {e}"),
            HostError::HostGone => f.write_str("the clove host closed the connection"),
            HostError::Protocol(m) => write!(f, "clove host protocol error: {m}"),
            HostError::Rpc { code, message } => write!(f, "clove host error {code}: {message}"),
            HostError::Tool { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for HostError {}

impl From<HostError> for CloveError {
    /// A tool failure keeps its code (so the plugin exits as the built-in
    /// would); any other failure is a daemon-class error.
    fn from(e: HostError) -> Self {
        match e {
            HostError::Tool {
                clove_code,
                exit,
                message,
            } => CloveError::Remote {
                code: clove_code,
                exit,
                message,
            },
            other => CloveError::Remote {
                code: "DAEMON_ERROR".to_owned(),
                exit: 7,
                message: other.to_string(),
            },
        }
    }
}

/// What the host reported in its `hello` reply.
#[derive(Debug, Clone)]
pub struct HostInfo {
    /// The host IPC protocol version.
    pub host_api: u32,
    /// The host's clove version.
    pub host_version: String,
    /// The item schema version the host writes.
    pub schema: u32,
    /// The `.clove/` directory the host serves.
    pub clove_dir: String,
    /// Every method the host answers; check before calling a newer one.
    pub methods: Vec<String>,
}

/// A connection to the host IPC channel.
pub struct HostClient {
    reader: BufReader<Stream>,
    next_id: u64,
    info: HostInfo,
}

impl HostClient {
    /// Connect to `socket`, authenticate with `token`, and read the host's
    /// `hello`. `expect_clove_dir`, when given, must match the directory the
    /// host serves (a guard against a stale environment from another run).
    pub fn connect(
        socket: &str,
        token: &str,
        client_name: &str,
        expect_clove_dir: Option<&str>,
    ) -> Result<HostClient, HostError> {
        let name = socket
            .to_fs_name::<GenericFilePath>()
            .map_err(HostError::Connect)?;
        let stream = Stream::connect(name).map_err(HostError::Connect)?;
        let mut client = HostClient {
            reader: BufReader::new(stream),
            next_id: 0,
            info: HostInfo {
                host_api: 0,
                host_version: String::new(),
                schema: 0,
                clove_dir: String::new(),
                methods: Vec::new(),
            },
        };
        let reply = client.call(
            "hello",
            json!({
                "token": token,
                "plugin_api": crate::CLOVE_PLUGIN_API,
                "client": { "name": client_name },
            }),
        )?;
        client.info = parse_hello(&reply)?;
        if let Some(expected) = expect_clove_dir {
            if client.info.clove_dir != expected {
                return Err(HostError::Protocol(format!(
                    "the host serves {} but this plugin runs for {expected}",
                    client.info.clove_dir
                )));
            }
        }
        Ok(client)
    }

    /// What the host reported when the connection opened.
    pub fn info(&self) -> &HostInfo {
        &self.info
    }

    /// Whether the host answers `method`.
    pub fn supports(&self, method: &str) -> bool {
        self.info.methods.iter().any(|m| m == method)
    }

    /// Call any method by name. The typed methods below wrap this.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, HostError> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = serde_json::to_vec(&request)
            .map_err(|e| HostError::Protocol(format!("cannot encode the request: {e}")))?;
        line.push(b'\n');
        let mut writer = self.reader.get_ref();
        // A failed write still reads on: a host that refuses a request (one
        // over its size cap, say) answers with the reason before it closes, and
        // that reason beats a bare "connection closed".
        let written = writer.write_all(&line).and_then(|()| writer.flush());

        let mut reply = Vec::new();
        match self.reader.read_until(b'\n', &mut reply) {
            Ok(0) | Err(_) => return Err(HostError::HostGone),
            Ok(_) if written.is_err() && !reply.ends_with(b"\n") => {
                return Err(HostError::HostGone)
            }
            Ok(_) => {}
        }
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|e| HostError::Protocol(format!("unreadable reply: {e}")))?;
        if let Some(error) = reply.get("error") {
            return Err(rpc_error(error));
        }
        if reply.get("id") != Some(&json!(id)) {
            return Err(HostError::Protocol(format!(
                "reply for request {:?}, expected {id}",
                reply.get("id")
            )));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| HostError::Protocol("a reply with neither result nor error".to_owned()))
    }

    fn typed<A: Serialize>(&mut self, method: &str, args: &A) -> Result<Value, HostError> {
        let params = serde_json::to_value(args)
            .map_err(|e| HostError::Protocol(format!("cannot encode the arguments: {e}")))?;
        self.call(method, params)
    }

    /// The repository's id prefix, default type, and the valid statuses,
    /// types, and priorities.
    pub fn config(&mut self) -> Result<Value, HostError> {
        self.call("config", json!({}))
    }

    /// Items matching the filters (MCP `clove_list`).
    pub fn list(&mut self, args: &ListArgs) -> Result<Value, HostError> {
        self.typed("list", args)
    }

    /// Items ready to start (MCP `clove_ready`).
    pub fn ready(&mut self, args: &FilterArgs) -> Result<Value, HostError> {
        self.typed("ready", args)
    }

    /// Items blocked by an open dependency (MCP `clove_blocked`).
    pub fn blocked(&mut self, args: &BlockedArgs) -> Result<Value, HostError> {
        self.typed("blocked", args)
    }

    /// One item (MCP `clove_show`).
    pub fn show(&mut self, args: &IdArgs) -> Result<Value, HostError> {
        self.typed("show", args)
    }

    /// Full-text search (MCP `clove_search`).
    pub fn search(&mut self, args: &SearchArgs) -> Result<Value, HostError> {
        self.typed("search", args)
    }

    /// An item's comments (MCP `clove_comments`).
    pub fn comments(&mut self, args: &CommentsArgs) -> Result<Value, HostError> {
        self.typed("comments", args)
    }

    /// An item's dependency tree (MCP `clove_dep_tree`).
    pub fn dep_tree(&mut self, args: &DepTreeArgs) -> Result<Value, HostError> {
        self.typed("dep_tree", args)
    }

    /// Repository statistics (MCP `clove_stats`).
    pub fn stats(&mut self, args: &StatsArgs) -> Result<Value, HostError> {
        self.typed("stats", args)
    }

    /// Create an item (MCP `clove_new`).
    pub fn create(&mut self, args: &NewArgs) -> Result<Value, HostError> {
        self.typed("create", args)
    }

    /// Change an item's status (MCP `clove_status`).
    pub fn set_status(&mut self, args: &StatusArgs) -> Result<Value, HostError> {
        self.typed("set_status", args)
    }

    /// Edit an item's fields in one write (MCP `clove_edit`).
    pub fn edit(&mut self, args: &EditArgs) -> Result<Value, HostError> {
        self.typed("edit", args)
    }

    /// Add a comment (MCP `clove_comment`).
    pub fn comment(&mut self, args: &CommentArgs) -> Result<Value, HostError> {
        self.typed("comment", args)
    }

    /// Add a hard dependency (MCP `clove_dep_add`).
    pub fn dep_add(&mut self, args: &DepAddArgs) -> Result<Value, HostError> {
        self.typed("dep_add", args)
    }

    /// Remove a hard dependency (MCP `clove_dep_remove`).
    pub fn dep_remove(&mut self, args: &DepAddArgs) -> Result<Value, HostError> {
        self.typed("dep_remove", args)
    }

    /// Set or clear an item's parent (MCP `clove_set_parent`).
    pub fn set_parent(&mut self, args: &SetParentArgs) -> Result<Value, HostError> {
        self.typed("set_parent", args)
    }
}

fn parse_hello(reply: &Value) -> Result<HostInfo, HostError> {
    let bad = |what: &str| HostError::Protocol(format!("hello reply without {what}"));
    let host_api = reply["host_api"].as_u64().ok_or_else(|| bad("host_api"))? as u32;
    if host_api != HOST_API {
        return Err(HostError::Protocol(format!(
            "host speaks protocol {host_api}, this plugin speaks {HOST_API}"
        )));
    }
    Ok(HostInfo {
        host_api,
        host_version: reply["host_version"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        schema: reply["schema"].as_u64().unwrap_or_default() as u32,
        clove_dir: reply["clove_dir"]
            .as_str()
            .ok_or_else(|| bad("clove_dir"))?
            .to_owned(),
        methods: reply["methods"]
            .as_array()
            .ok_or_else(|| bad("methods"))?
            .iter()
            .filter_map(|m| m.as_str().map(str::to_owned))
            .collect(),
    })
}

fn rpc_error(error: &Value) -> HostError {
    let message = error["message"]
        .as_str()
        .unwrap_or("unknown error")
        .to_owned();
    match error["data"]["clove_code"].as_str() {
        // A tool failure, or invalid params (classified as a validation error).
        Some(code) => HostError::Tool {
            clove_code: code.to_owned(),
            exit: error["data"]["exit"].as_u64().unwrap_or(7) as u8,
            message,
        },
        None => HostError::Rpc {
            code: error["code"].as_i64().unwrap_or(0),
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_error_keeps_its_clove_code() {
        let e = rpc_error(&json!({
            "code": -32000,
            "message": "ITEM_NOT_FOUND: no item",
            "data": { "clove_code": "ITEM_NOT_FOUND", "exit": 2 }
        }));
        let clove: CloveError = e.into();
        assert_eq!(clove_types::error_code(&clove), ("ITEM_NOT_FOUND", 2));
    }

    #[test]
    fn a_protocol_error_is_a_daemon_error() {
        let e = rpc_error(&json!({ "code": -32001, "message": "bad token" }));
        assert!(matches!(e, HostError::Rpc { code: -32001, .. }));
        let clove: CloveError = e.into();
        assert_eq!(clove_types::error_code(&clove), ("DAEMON_ERROR", 7));
    }

    #[test]
    fn hello_must_speak_this_protocol() {
        let ok = json!({
            "host_api": HOST_API, "host_version": "0.2.0", "schema": 1,
            "clove_dir": "/r/.clove", "methods": ["hello", "list"]
        });
        let info = parse_hello(&ok).unwrap();
        assert_eq!(info.clove_dir, "/r/.clove");
        assert_eq!(info.methods, ["hello", "list"]);

        let newer = json!({ "host_api": HOST_API + 1, "clove_dir": "/r", "methods": [] });
        assert!(matches!(parse_hello(&newer), Err(HostError::Protocol(_))));
    }
}
