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
            Some(path) => RedDBRuntime::with_options(RedDBOptions::persistent(&path))
                .map_err(|e| Error::from_reason(format!("open {path}: {e}")))?,
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
