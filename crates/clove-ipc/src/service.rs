//! The typed CLI<->daemon RPC service (tarpc).
//!
//! Phase 0 scaffold: a single `ping` method that proves the `tarpc` service
//! macro compiles on the workspace toolchain. The full method set (queries,
//! graph ops, and the M4 mutations) is filled in during the IPC refactor.

/// The clove daemon RPC service. `tarpc::service` generates the `CloveRpcClient`
/// and the `CloveRpc` server trait from this definition.
#[tarpc::service]
pub trait CloveRpc {
    /// Liveness probe: returns the daemon's protocol version string.
    async fn ping() -> String;
}
