//! The host IPC channel a plugin can call back on (`PLUGIN_SYSTEM.md` §6.6).
//!
//! While a plugin runs, the host serves the clove tool surface
//! ([`clove_service::Engine`], the same engine as the MCP server) over a
//! per-invocation local socket: newline-delimited JSON-RPC 2.0. A plugin that
//! reads or writes items this way never links an AGPL crate and never opens the
//! store itself, so every write still goes through the unified write path (the
//! `cloved` daemon when one runs, else in-process `clove_core::ops`).
//!
//! Who may connect:
//! - **Unix:** a filesystem socket inside a fresh `0700` directory (the kernel
//!   refuses other users), plus a peer-uid check at accept.
//! - **Windows:** a named pipe with an owner-only security descriptor, plus a
//!   client-SID check after the first message.
//! - **Everywhere:** the first message must be `hello` carrying the 256-bit
//!   token from `$CLOVE_HOST_TOKEN`, compared in constant time. The token stays
//!   valid for the whole invocation, so a plugin's own helper processes (which
//!   inherit its environment) may connect too.
//!
//! Requests on one connection are answered in order; each connection has its own
//! thread. No tokio runtime runs on these threads: the engine's daemon client
//! drives its own (`block_on`).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_service::{Engine, StoreService, ToolError};
use clove_types::tool::*;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Stream};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::plugin::PluginGlobals;

/// The host IPC protocol version, reported by `hello`. Bumped only on a breaking
/// change to a method's params or result; new methods are additive.
pub const HOST_API: u32 = 1;

/// The largest request line the host reads (16 MiB). A longer one is answered
/// with an error and the connection is closed. Responses are not capped: page
/// large reads with `limit`/`offset`.
const MAX_REQUEST: u64 = 16 * 1024 * 1024;

/// How long a new connection has to send its whole `hello` (Unix; Windows
/// named pipes in interprocess have no receive timeout, so there an idle
/// unauthenticated connection holds its thread until the plugin exits).
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// [`HELLO_TIMEOUT`], or `CLOVE_PLUGIN_HELLO_MS` in a `test-knobs` build.
fn hello_timeout() -> Duration {
    crate::knobs::duration("CLOVE_PLUGIN_HELLO_MS").unwrap_or(HELLO_TIMEOUT)
}

/// The largest `hello` line the host reads before authentication.
const MAX_HELLO: usize = 64 * 1024;

/// Every method the host answers, in the order `hello` reports them.
const METHODS: &[&str] = &[
    "hello",
    "config",
    "list",
    "ready",
    "blocked",
    "show",
    "search",
    "comments",
    "dep_tree",
    "stats",
    "create",
    "set_status",
    "edit",
    "comment",
    "dep_add",
    "dep_remove",
    "set_parent",
    "store.scan",
    "store.scan_frontmatter",
    "store.get",
    "store.new_id",
    "store.create_item",
    "store.apply_edit",
    "store.dep_add",
    "store.dep_remove",
    "store.link_external",
    "store.comments",
    "store.add_comment",
];

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// A tool call that ran and failed; `data` carries the clove code and exit.
const TOOL_ERROR: i64 = -32000;
/// The connection did not authenticate.
const UNAUTHORIZED: i64 = -32001;

/// A running host IPC server. Dropping it does not stop the accept thread; the
/// host process exits right after the plugin does, which ends it.
pub struct HostServer {
    /// The socket path (Unix) or pipe path (Windows) the plugin connects to.
    socket: String,
    token: String,
    /// The private directory holding the Unix socket, removed by [`cleanup`].
    #[cfg_attr(not(unix), allow(dead_code))]
    dir: Option<Utf8PathBuf>,
}

impl HostServer {
    /// Bind a fresh socket and start accepting. The engine is built on the
    /// first request, so a plugin that never calls back costs one bind.
    pub fn start(ctx: &Ctx, globals: &PluginGlobals) -> std::io::Result<HostServer> {
        let token = random_hex(32)?;
        let (socket, dir) = socket_path()?;
        let name = socket.clone().to_fs_name::<GenericFilePath>()?;
        let listener = match owner_only(ListenerOptions::new().name(name))?.create_sync() {
            Ok(listener) => listener,
            Err(e) => {
                if let Some(dir) = &dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                return Err(e);
            }
        };

        let shared = Arc::new(Shared {
            token: token.clone(),
            clove_dir: ctx.clove_dir.clone(),
            repo_root: ctx.root.clone(),
            id_prefix: ctx.config.id_prefix.clone(),
            default_type: ctx.config.default_type,
            tiers: clove_engine::Tiers {
                daemon: !globals.no_index,
                index: !globals.no_index,
                deep: globals.deep,
                auto_refresh: ctx.config.index.auto_refresh,
            },
            engine: OnceLock::new(),
            store: OnceLock::new(),
        });
        std::thread::Builder::new()
            .name("clove-plugin-host".to_owned())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else {
                        // EMFILE and friends repeat; do not spin on them.
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    };
                    let shared = shared.clone();
                    let _ = std::thread::Builder::new()
                        .name("clove-plugin-conn".to_owned())
                        .spawn(move || serve(conn, &shared));
                }
            })?;
        Ok(HostServer { socket, token, dir })
    }

    /// Export the channel to the plugin (`$CLOVE_HOST_SOCKET`, `$CLOVE_HOST_TOKEN`).
    pub fn export(&self, cmd: &mut Command) {
        cmd.env("CLOVE_HOST_SOCKET", &self.socket);
        cmd.env("CLOVE_HOST_TOKEN", &self.token);
    }

    /// Remove the socket's private directory (Unix). Called after the plugin
    /// exits, before the host does: `process::exit` skips destructors.
    pub fn cleanup(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Clear the channel variables, so a `clove` run inside a plugin never hands
/// the *outer* host's socket to its own plugin when its server did not start.
pub fn clear_env(cmd: &mut Command) {
    cmd.env_remove("CLOVE_HOST_SOCKET");
    cmd.env_remove("CLOVE_HOST_TOKEN");
}

/// What every connection thread shares.
struct Shared {
    token: String,
    clove_dir: Utf8PathBuf,
    repo_root: Utf8PathBuf,
    id_prefix: String,
    default_type: clove_types::ItemType,
    tiers: clove_engine::Tiers,
    engine: OnceLock<Engine>,
    store: OnceLock<StoreService>,
}

impl Shared {
    fn store(&self) -> &StoreService {
        self.store.get_or_init(|| {
            StoreService::new(
                self.clove_dir.clone(),
                self.repo_root.clone(),
                self.id_prefix.clone(),
            )
        })
    }

    fn engine(&self) -> &Engine {
        self.engine.get_or_init(|| {
            Engine::new(
                self.clove_dir.clone(),
                self.repo_root.clone(),
                self.id_prefix.clone(),
                self.default_type,
                self.tiers,
            )
        })
    }
}

/// Serve one connection until it closes, fails authentication, or sends an
/// unusable frame.
fn serve(conn: Stream, shared: &Shared) {
    if !peer_is_this_user_early(&conn) {
        return;
    }
    let mut reader = BufReader::new(&conn);
    let mut writer = &conn;
    let mut authed = false;
    let mut line = Vec::new();
    let timeout = hello_timeout();
    let deadline = Instant::now() + timeout;
    loop {
        line.clear();
        if !authed {
            match read_hello_line(&mut reader, &conn, deadline, &mut line) {
                HelloRead::Line => {}
                HelloRead::Closed => return,
                HelloRead::TimedOut => {
                    let message = if timeout == HELLO_TIMEOUT {
                        format!("no hello within {} seconds", HELLO_TIMEOUT.as_secs())
                    } else {
                        format!("no hello within {} ms", timeout.as_millis())
                    };
                    let _ = send(
                        &mut writer,
                        &error(Value::Null, UNAUTHORIZED, &message, None),
                    );
                    return;
                }
                HelloRead::TooLarge => {
                    drain_line(&mut reader, Some(deadline));
                    let _ = send(
                        &mut writer,
                        &error(Value::Null, UNAUTHORIZED, "hello larger than 64 KiB", None),
                    );
                    return;
                }
            }
        } else {
            match (&mut reader)
                .take(MAX_REQUEST + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
        if line.len() as u64 > MAX_REQUEST {
            if !line.ends_with(b"\n") {
                drain_line(&mut reader, None);
            }
            let _ = send(
                &mut writer,
                &error(
                    Value::Null,
                    INVALID_REQUEST,
                    "request larger than 16 MiB",
                    None,
                ),
            );
            return;
        }
        let request: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = send(
                    &mut writer,
                    &error(Value::Null, PARSE_ERROR, &format!("parse error: {e}"), None),
                );
                if !authed {
                    return;
                }
                continue;
            }
        };
        let Some(obj) = request.as_object() else {
            // Batches (arrays) and bare values are not supported.
            let _ = send(
                &mut writer,
                &error(
                    Value::Null,
                    INVALID_REQUEST,
                    "expected one JSON-RPC request object",
                    None,
                ),
            );
            if !authed {
                return;
            }
            continue;
        };
        // A request without an id is a notification: run it, send nothing.
        let id = obj.get("id").cloned();
        let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
        let params = obj.get("params").cloned().unwrap_or(Value::Null);

        if !authed {
            match hello(&conn, shared, method, &params) {
                Ok(reply) => {
                    authed = true;
                    let _ = conn.set_recv_timeout(None);
                    if let Some(id) = id {
                        if send(&mut writer, &success(id, reply)).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                Err(message) => {
                    let _ = send(
                        &mut writer,
                        &error(id.unwrap_or(Value::Null), UNAUTHORIZED, &message, None),
                    );
                    return;
                }
            }
        }

        let reply = match dispatch(shared, method, params) {
            Ok(result) => id.map(|id| success(id, result)),
            Err(e) => id.map(|id| error(id, e.code, &e.message, e.data)),
        };
        if let Some(reply) = reply {
            if send(&mut writer, &reply).is_err() {
                return;
            }
        }
    }
}

/// How reading the unauthenticated first line ended.
enum HelloRead {
    Line,
    Closed,
    TimedOut,
    TooLarge,
}

/// Read the first line under one overall deadline (not a per-read timeout, so a
/// client trickling bytes cannot stay unauthenticated) and a 64 KiB cap.
fn read_hello_line(
    reader: &mut BufReader<&Stream>,
    conn: &Stream,
    deadline: Instant,
    line: &mut Vec<u8>,
) -> HelloRead {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return HelloRead::TimedOut;
        }
        let _ = conn.set_recv_timeout(Some(left));
        let chunk = match reader.fill_buf() {
            Ok([]) => return HelloRead::Closed,
            Ok(chunk) => chunk,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return HelloRead::TimedOut
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return HelloRead::Closed,
        };
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (chunk.len(), false),
        };
        line.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if line.len() > MAX_HELLO {
            return HelloRead::TooLarge;
        }
        if done {
            return HelloRead::Line;
        }
    }
}

/// Read and drop the rest of a refused line, until its newline, the end of
/// the stream, or `deadline`.
///
/// The client is still writing that line. On Windows, `flush` waits until the
/// client has read the reply, and the client reads only once its write is done,
/// so answering before the line is consumed deadlocks both sides.
fn drain_line(reader: &mut BufReader<&Stream>, deadline: Option<Instant>) {
    loop {
        if let Some(deadline) = deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            let _ = reader.get_ref().set_recv_timeout(Some(left));
        }
        let (take, done) = match reader.fill_buf() {
            Ok([]) => return,
            Ok(chunk) => match chunk.iter().position(|&b| b == b'\n') {
                Some(at) => (at + 1, true),
                None => (chunk.len(), false),
            },
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        reader.consume(take);
        if done {
            return;
        }
    }
}

/// Authenticate the connection's first message.
fn hello(conn: &Stream, shared: &Shared, method: &str, params: &Value) -> Result<Value, String> {
    if method != "hello" {
        return Err("the first message must be `hello`".to_owned());
    }
    let token = params.get("token").and_then(Value::as_str).unwrap_or("");
    if !constant_time_eq(token.as_bytes(), shared.token.as_bytes()) {
        return Err("bad token".to_owned());
    }
    if !peer_is_this_user_late(conn) {
        return Err("the client is not running as this user".to_owned());
    }
    Ok(json!({
        "host_api": HOST_API,
        "host_version": env!("CARGO_PKG_VERSION"),
        "schema": clove_types::CURRENT_SCHEMA_VERSION,
        "clove_dir": shared.clove_dir.as_str(),
        "methods": METHODS,
    }))
}

/// A failed method call, as a JSON-RPC error.
struct RpcFailure {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl From<ToolError> for RpcFailure {
    fn from(e: ToolError) -> Self {
        let (clove_code, exit) = e.code();
        RpcFailure {
            code: TOOL_ERROR,
            message: e.to_string(),
            data: Some(json!({ "clove_code": clove_code, "exit": exit })),
        }
    }
}

/// Run one method on the engine.
fn dispatch(shared: &Shared, method: &str, params: Value) -> Result<Value, RpcFailure> {
    let engine = || shared.engine();
    match method {
        "hello" => Err(RpcFailure {
            code: INVALID_REQUEST,
            message: "already authenticated".to_owned(),
            data: None,
        }),
        "config" => Ok(json!({
            "id_prefix": shared.id_prefix,
            "default_type": shared.default_type,
            "types": ["bug", "feature", "chore", "docs", "epic"],
            "statuses": ["open", "in_progress", "closed"],
            "priorities": [0, 1, 2, 3, 4],
        })),
        "list" => call(params, |a: ListArgs| engine().list(a)),
        "ready" => call(params, |a: FilterArgs| engine().ready(a)),
        "blocked" => call(params, |a: BlockedArgs| engine().blocked(a)),
        "show" => call(params, |a: IdArgs| engine().show(a)),
        "search" => call(params, |a: SearchArgs| engine().search(a)),
        "comments" => call(params, |a: CommentsArgs| engine().comments(a)),
        "dep_tree" => call(params, |a: DepTreeArgs| engine().dep_tree(a)),
        "stats" => call(params, |a: StatsArgs| engine().stats(a)),
        "create" => call(params, |a: NewArgs| engine().create(a)),
        "set_status" => call(params, |a: StatusArgs| engine().set_status(a)),
        "edit" => call(params, |a: EditArgs| engine().edit(a)),
        "comment" => call(params, |a: CommentArgs| engine().comment(a)),
        "dep_add" => call(params, |a: DepAddArgs| engine().dep_add(a)),
        "dep_remove" => call(params, |a: DepAddArgs| engine().dep_remove(a)),
        "set_parent" => call(params, |a: SetParentArgs| engine().set_parent(a)),
        // The store tier (importers, sync engines): whole items, own timestamps.
        "store.scan" => shared.store().scan().map_err(RpcFailure::from),
        "store.scan_frontmatter" => shared.store().scan_frontmatter().map_err(RpcFailure::from),
        "store.get" => call(params, |p: StoreIdParams| shared.store().get(p)),
        "store.new_id" => shared.store().new_id().map_err(RpcFailure::from),
        "store.create_item" => call(params, |p: CreateItemParams| shared.store().create_item(p)),
        "store.apply_edit" => call(params, |p: ApplyEditParams| shared.store().apply_edit(p)),
        "store.dep_add" => call(params, |p: StoreDepParams| shared.store().dep_add(p)),
        "store.dep_remove" => call(params, |p: StoreDepParams| shared.store().dep_remove(p)),
        "store.link_external" => call(params, |p: LinkExternalParams| {
            shared.store().link_external(p)
        }),
        "store.comments" => call(params, |p: StoreIdParams| shared.store().comments(p)),
        "store.add_comment" => call(params, |p: AddCommentParams| shared.store().add_comment(p)),
        other => Err(RpcFailure {
            code: METHOD_NOT_FOUND,
            message: format!("unknown method `{other}`"),
            data: None,
        }),
    }
}

/// Decode `params` (absent → `{}`) into the tool's argument type and run it.
fn call<A: DeserializeOwned>(
    params: Value,
    run: impl FnOnce(A) -> Result<Value, ToolError>,
) -> Result<Value, RpcFailure> {
    let params = if params.is_null() { json!({}) } else { params };
    let args = serde_json::from_value(params).map_err(|e| RpcFailure {
        code: INVALID_PARAMS,
        message: format!("invalid params: {e}"),
        data: Some(json!({ "clove_code": "VALIDATION_ERROR", "exit": 4 })),
    })?;
    run(args).map_err(RpcFailure::from)
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(data) = data {
        err["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

fn send(writer: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(std::io::Error::other)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// A fresh private directory with the socket inside it (Unix).
#[cfg(unix)]
fn socket_path() -> std::io::Result<(String, Option<Utf8PathBuf>)> {
    use std::os::unix::fs::DirBuilderExt;
    // sun_path is 104 bytes on macOS, 108 on Linux; a long $TMPDIR (macOS)
    // falls back to /tmp.
    const SUN_PATH_MAX: usize = 100;
    let suffix = format!("clove-plugin-{}", random_hex(8)?);
    let mut base = std::env::temp_dir();
    if base.join(&suffix).join("s").as_os_str().len() > SUN_PATH_MAX {
        base = std::path::PathBuf::from("/tmp");
    }
    let dir = Utf8PathBuf::from_path_buf(base.join(&suffix))
        .map_err(|_| std::io::Error::other("the temp directory is not UTF-8"))?;
    // `create` (not `create_all`) fails if the name exists: never reuse a
    // directory someone else made.
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let sock = dir.join("s");
    Ok((sock.into_string(), Some(dir)))
}

/// A unique named pipe (Windows).
#[cfg(windows)]
fn socket_path() -> std::io::Result<(String, Option<Utf8PathBuf>)> {
    Ok((
        format!(
            r"\\.\pipe\clove-plugin-{}-{}",
            std::process::id(),
            random_hex(8)?
        ),
        None,
    ))
}

/// Unix: the pipe's DACL is the directory's mode. Windows: a protected DACL
/// that admits only this user's SID (the default grants Everyone read access).
#[cfg(windows)]
fn owner_only(options: ListenerOptions<'_>) -> std::io::Result<ListenerOptions<'_>> {
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    let sid = clove_ipc::win::current_user_sid()?;
    let sddl = widestring::U16CString::from_str(format!("O:{sid}D:P(A;;GA;;;{sid})"))
        .map_err(std::io::Error::other)?;
    let descriptor = SecurityDescriptor::deserialize(&sddl)?;
    Ok(options.security_descriptor(descriptor))
}

#[cfg(not(windows))]
fn owner_only(options: ListenerOptions<'_>) -> std::io::Result<ListenerOptions<'_>> {
    Ok(options)
}

/// Unix: check the peer's uid at accept (the socket's directory already keeps
/// other users out; this is the second layer).
#[cfg(unix)]
fn peer_is_this_user_early(conn: &Stream) -> bool {
    use interprocess::local_socket::traits::StreamCommon as _;
    // SAFETY: geteuid cannot fail.
    let uid = unsafe { libc::geteuid() };
    conn.peer_creds()
        .map(|creds| creds.euid() == Some(uid))
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn peer_is_this_user_early(_conn: &Stream) -> bool {
    true
}

/// Windows: a pipe client's SID can be read only once it has sent data, so the
/// check runs on the `hello`.
#[cfg(windows)]
fn peer_is_this_user_late(conn: &Stream) -> bool {
    use std::os::windows::io::AsHandle as _;
    let Stream::NamedPipe(pipe) = conn;
    match (
        clove_ipc::win::pipe_client_sid(pipe.as_handle()),
        clove_ipc::win::current_user_sid(),
    ) {
        (Ok(client), Ok(me)) => client == me,
        _ => false,
    }
}

#[cfg(not(windows))]
fn peer_is_this_user_late(_conn: &Stream) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_in_constant_time_and_exactly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn a_token_is_256_bits_of_hex() {
        let token = random_hex(32).unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, random_hex(32).unwrap());
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let shared = Shared {
            token: String::new(),
            clove_dir: Utf8PathBuf::from("/nonexistent/.clove"),
            repo_root: Utf8PathBuf::from("/nonexistent"),
            id_prefix: "proj".to_owned(),
            default_type: clove_types::ItemType::Feature,
            tiers: clove_engine::Tiers::files_only(),
            engine: OnceLock::new(),
            store: OnceLock::new(),
        };
        let err = dispatch(&shared, "frobnicate", Value::Null).unwrap_err();
        assert_eq!(err.code, METHOD_NOT_FOUND);
        let err = dispatch(&shared, "show", json!({ "nope": 1 })).unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert_eq!(err.data.unwrap()["clove_code"], "VALIDATION_ERROR");
    }

    #[test]
    fn hello_reports_every_method_dispatch_knows() {
        for method in METHODS {
            assert!(
                *method == "hello"
                    || !matches!(
                        dispatch(
                            &Shared {
                                token: String::new(),
                                clove_dir: Utf8PathBuf::from("/nonexistent/.clove"),
                                repo_root: Utf8PathBuf::from("/nonexistent"),
                                id_prefix: "proj".to_owned(),
                                default_type: clove_types::ItemType::Feature,
                                tiers: clove_engine::Tiers::files_only(),
                                engine: OnceLock::new(),
                                store: OnceLock::new(),
                            },
                            method,
                            json!({ "unknown_field_only": true }),
                        ),
                        Err(RpcFailure {
                            code: METHOD_NOT_FOUND,
                            ..
                        })
                    ),
                "{method} is advertised but not dispatched"
            );
        }
    }
}
