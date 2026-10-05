//! Node addon: the RedDB engine, in process.
//!
//! `@reddb-io/sdk` used to spawn `red rpc --stdio` and talk JSON-RPC over the
//! child's pipes. This addon serves the *same* protocol — same methods, same
//! transaction and cursor semantics, because it drives the same dispatcher
//! ([`reddb::rpc_stdio::EmbeddedRpcSession`]) — but as a function call, so
//! embedded mode needs no subprocess and no separate binary on `PATH`.
//!
//! The JS side is a thin transport: `call(requestLine) -> Promise<responseLine>`.

use std::sync::{Arc, Mutex};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use reddb::api::RedDBOptions;
use reddb::operational_bootstrap::{resolve_operational_bootstrap, OperationalBootstrapInput};
use reddb::rpc_stdio::EmbeddedRpcSession;
use reddb::RedDBRuntime;

/// Everything an open engine owns. `close` takes both out so the embedded
/// writer lock is released deterministically instead of at GC time.
struct Inner {
    state: Mutex<Option<(EmbeddedRpcSession, Arc<RedDBRuntime>)>>,
}

impl Inner {
    fn call(&self, request: &str) -> Result<String> {
        let mut state = self.state.lock().map_err(|_| closed_error())?;
        let (session, _) = state.as_mut().ok_or_else(closed_error)?;
        let (response, closed) = session.handle(request);
        if closed {
            Self::shutdown(&mut state);
        }
        Ok(response)
    }

    fn shutdown(state: &mut Option<(EmbeddedRpcSession, Arc<RedDBRuntime>)>) {
        if let Some((session, runtime)) = state.take() {
            // Drop the session first (discards an open transaction), then
            // flush: the runtime does not checkpoint on drop.
            drop(session);
            let _ = runtime.checkpoint();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            Self::shutdown(&mut state);
        }
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_truthy(name: &str) -> bool {
    env(name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Options for a file-backed engine, resolved the way `red rpc --stdio --path`
/// does it with no flags: the storage profile comes from the `REDDB_STORAGE_*`
/// environment (and the built-in default when unset), with no topology and no
/// config file. The subprocess this addon replaces inherited the app's
/// environment, so those variables keep working, and an invalid value fails
/// the open instead of being ignored. Mirrors `operational_bootstrap_input` in
/// `src/bin/red.rs`.
fn persistent_options(path: &str) -> std::result::Result<RedDBOptions, String> {
    let plan = resolve_operational_bootstrap(OperationalBootstrapInput {
        storage_preset: env("REDDB_STORAGE_PRESET"),
        storage_profile: env("REDDB_STORAGE_PROFILE").or_else(|| env("REDDB_DEPLOY_PROFILE")),
        storage_packaging: env("REDDB_STORAGE_PACKAGING"),
        replica_count: env("REDDB_REPLICA_COUNT"),
        managed_backup: env_truthy("REDDB_MANAGED_BACKUP"),
        wal_retention: env_truthy("REDDB_WAL_RETENTION"),
        ..Default::default()
    })?;
    RedDBOptions::persistent(path)
        .with_storage_profile(plan.storage_profile)
        .map_err(|e| format!("storage profile: {e}"))
}

fn closed_error() -> Error {
    Error::from_reason("RedDB engine is closed")
}

/// An open embedded engine.
#[napi]
pub struct Engine {
    inner: Arc<Inner>,
}

#[napi]
impl Engine {
    /// Open the database at `path`, or an in-memory one when `path` is
    /// omitted. Opening a file takes the single-writer lock; it throws if
    /// another process holds it.
    #[napi(factory)]
    pub fn open(path: Option<String>) -> Result<Engine> {
        let runtime = match path.filter(|p| !p.is_empty()) {
            Some(path) => {
                let options = persistent_options(&path).map_err(Error::from_reason)?;
                RedDBRuntime::with_options(options)
                    .map_err(|e| Error::from_reason(format!("open {path}: {e}")))?
            }
            None => RedDBRuntime::in_memory().map_err(|e| Error::from_reason(e.to_string()))?,
        };
        let runtime = Arc::new(runtime);
        let session = EmbeddedRpcSession::new(runtime.clone());
        Ok(Engine {
            inner: Arc::new(Inner {
                state: Mutex::new(Some((session, runtime))),
            }),
        })
    }

    /// Serve one JSON-RPC 2.0 request (a single JSON document, no trailing
    /// newline) and resolve with the single-line JSON response. Runs on the
    /// libuv thread pool, so a long query does not block the event loop.
    /// Requests on one engine are served one at a time, but not necessarily
    /// in call order if several are in flight; a caller that needs ordering
    /// across un-awaited calls must chain them (the SDK's client does).
    #[napi(ts_return_type = "Promise<string>")]
    pub fn call(&self, request: String) -> AsyncTask<CallTask> {
        AsyncTask::new(CallTask {
            inner: self.inner.clone(),
            request,
        })
    }

    /// Synchronous variant of [`call`](Self::call).
    #[napi]
    pub fn call_sync(&self, request: String) -> Result<String> {
        self.inner.call(&request)
    }

    /// Discard any open transaction, flush to disk and release the file
    /// lock. Idempotent; the `close` RPC method does the same.
    #[napi]
    pub fn close(&self) -> Result<()> {
        let mut state = self.inner.state.lock().map_err(|_| closed_error())?;
        Inner::shutdown(&mut state);
        Ok(())
    }
}

pub struct CallTask {
    inner: Arc<Inner>,
    request: String,
}

impl Task for CallTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        self.inner.call(&self.request)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}
