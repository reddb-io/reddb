//! Stable C ABI for embedding the RedDB engine in language drivers.
//!
//! The ABI deliberately exposes the same JSON-RPC session used by the native
//! Node addon and `red rpc --stdio`. Language packages own URI parsing and
//! typed result conversion; engine semantics stay in one Rust implementation.

use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use reddb_server::api::RedDBOptions;
use reddb_server::json::{self, Map as JsonMap, Value as JsonValue};
use reddb_server::rpc_stdio::EmbeddedRpcSession;
use reddb_server::RedDBRuntime;

const ABI_VERSION: u32 = 1;
const EMBEDDED_WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReddbStatus {
    Ok = 0,
    InvalidArgument = 1,
    OpenError = 2,
    CallError = 3,
    InternalError = 4,
}

struct SessionState {
    session: EmbeddedRpcSession,
    runtime: Arc<RedDBRuntime>,
}

enum WorkerCommand {
    Call {
        request: String,
        reply: SyncSender<CallReply>,
    },
    Shutdown {
        reply: SyncSender<Result<(), String>>,
    },
}

struct CallReply {
    response: Result<String, String>,
    closed: bool,
}

struct Worker {
    sender: SyncSender<WorkerCommand>,
    join: Option<JoinHandle<()>>,
}

#[repr(C)]
pub struct ReddbHandle {
    worker: Mutex<Option<Worker>>,
}

impl ReddbHandle {
    fn shutdown(&self) -> Result<(), String> {
        let mut slot = self
            .worker
            .lock()
            .map_err(|_| "embedded session lock is poisoned".to_string())?;
        let Some(mut worker) = slot.take() else {
            return Ok(());
        };
        drop(slot);

        let (reply_tx, reply_rx) = sync_channel(0);
        if worker
            .sender
            .send(WorkerCommand::Shutdown { reply: reply_tx })
            .is_err()
        {
            join_worker(&mut worker)?;
            return Err("embedded worker stopped before shutdown".to_string());
        }
        let result = reply_rx
            .recv()
            .map_err(|_| "embedded worker stopped during shutdown".to_string())?;
        join_worker(&mut worker)?;
        result
    }
}

impl Drop for ReddbHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn open_session(path: Option<&str>) -> Result<SessionState, String> {
    let runtime = match path {
        Some(path) => RedDBRuntime::with_options(RedDBOptions::persistent(path))
            .map_err(|error| format!("open {path}: {error}"))?,
        None => RedDBRuntime::in_memory().map_err(|error| error.to_string())?,
    };
    let runtime = Arc::new(runtime);
    Ok(SessionState {
        session: EmbeddedRpcSession::new(Arc::clone(&runtime)),
        runtime,
    })
}

fn spawn_worker(path: Option<String>) -> Result<Worker, String> {
    let (sender, receiver) = sync_channel(0);
    let (ready_tx, ready_rx) = sync_channel(0);
    let join = thread::Builder::new()
        .name("reddb-ffi".to_string())
        .stack_size(EMBEDDED_WORKER_STACK_BYTES)
        .spawn(move || {
            let mut state = match open_session(path.as_deref()) {
                Ok(state) => {
                    if ready_tx.send(Ok(())).is_err() {
                        return;
                    }
                    Some(state)
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };

            while let Ok(command) = receiver.recv() {
                match command {
                    WorkerCommand::Call { request, reply } => {
                        let Some(current) = state.as_mut() else {
                            let _ = reply.send(CallReply {
                                response: Err("embedded database is closed".to_string()),
                                closed: true,
                            });
                            break;
                        };
                        let (response, closed) = current.session.handle(&request);
                        let response = if closed {
                            shutdown_locked(&mut state).map(|_| response)
                        } else {
                            Ok(response)
                        };
                        let _ = reply.send(CallReply { response, closed });
                        if closed {
                            break;
                        }
                    }
                    WorkerCommand::Shutdown { reply } => {
                        let result = shutdown_locked(&mut state);
                        let _ = reply.send(result);
                        break;
                    }
                }
            }
            let _ = shutdown_locked(&mut state);
        })
        .map_err(|error| format!("start embedded worker: {error}"))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Worker {
            sender,
            join: Some(join),
        }),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            let _ = join.join();
            Err("embedded worker stopped during startup".to_string())
        }
    }
}

fn join_worker(worker: &mut Worker) -> Result<(), String> {
    let Some(join) = worker.join.take() else {
        return Ok(());
    };
    join.join()
        .map_err(|_| "panic inside the embedded RedDB worker".to_string())
}

fn shutdown_locked(state: &mut Option<SessionState>) -> Result<(), String> {
    let Some(SessionState { session, runtime }) = state.take() else {
        return Ok(());
    };
    // An open transaction belongs to the session and must be discarded before
    // the runtime checkpoints committed state.
    drop(session);
    runtime
        .checkpoint()
        .map_err(|error| format!("checkpoint embedded database: {error}"))
}

fn write_error(out_error: *mut *mut c_char, message: impl AsRef<str>) {
    if out_error.is_null() {
        return;
    }
    let sanitized = message.as_ref().replace('\0', "�");
    let value = CString::new(sanitized).expect("NUL bytes were replaced");
    // SAFETY: the caller supplied a non-null output slot. Ownership of the
    // allocation is transferred and documented in `reddb.h`.
    unsafe { ptr::write(out_error, value.into_raw()) };
}

fn clear_output<T>(output: *mut *mut T) {
    if !output.is_null() {
        // SAFETY: the caller supplied a non-null output slot.
        unsafe { ptr::write(output, ptr::null_mut()) };
    }
}

fn ffi_status(
    out_error: *mut *mut c_char,
    operation: impl FnOnce() -> Result<(), (ReddbStatus, String)>,
) -> ReddbStatus {
    clear_output(out_error);
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => ReddbStatus::Ok,
        Ok(Err((status, message))) => {
            write_error(out_error, message);
            status
        }
        Err(_) => {
            write_error(out_error, "panic inside the embedded RedDB ABI");
            ReddbStatus::InternalError
        }
    }
}

fn call_session(handle: &ReddbHandle, request: &str) -> Result<String, (ReddbStatus, String)> {
    let mut slot = handle.worker.lock().map_err(|_| {
        (
            ReddbStatus::InternalError,
            "embedded session lock is poisoned".to_string(),
        )
    })?;
    let worker = slot.as_mut().ok_or_else(|| {
        (
            ReddbStatus::CallError,
            "embedded database is closed".to_string(),
        )
    })?;
    let (reply_tx, reply_rx) = sync_channel(0);
    worker
        .sender
        .send(WorkerCommand::Call {
            request: request.to_string(),
            reply: reply_tx,
        })
        .map_err(|_| {
            (
                ReddbStatus::InternalError,
                "embedded worker stopped before the call".to_string(),
            )
        })?;
    let reply = reply_rx.recv().map_err(|_| {
        (
            ReddbStatus::InternalError,
            "embedded worker stopped during the call".to_string(),
        )
    })?;
    if reply.closed {
        let mut worker = slot.take().expect("worker exists while handling a call");
        drop(slot);
        join_worker(&mut worker).map_err(|error| (ReddbStatus::InternalError, error))?;
    }
    reply
        .response
        .map_err(|error| (ReddbStatus::CallError, error))
}

fn write_string_output(
    output: *mut *mut c_char,
    value: String,
) -> Result<(), (ReddbStatus, String)> {
    let value = CString::new(value).map_err(|_| {
        (
            ReddbStatus::InternalError,
            "engine returned a string containing a NUL byte".to_string(),
        )
    })?;
    // SAFETY: callers validate output slots before reaching this helper.
    unsafe { ptr::write(output, value.into_raw()) };
    Ok(())
}

/// Return the version of the C ABI implemented by this library.
#[no_mangle]
pub extern "C" fn reddb_abi_version() -> u32 {
    ABI_VERSION
}

/// Open an in-memory engine (`path == NULL` or empty) or one `.rdb` file.
///
/// # Safety
///
/// `path`, when non-null, must point to a NUL-terminated byte string.
/// `out_handle` must point to writable storage for one pointer. `out_error`,
/// when non-null, must point to writable storage for one pointer.
#[no_mangle]
pub unsafe extern "C" fn reddb_open(
    path: *const c_char,
    out_handle: *mut *mut ReddbHandle,
    out_error: *mut *mut c_char,
) -> c_int {
    clear_output(out_handle);
    ffi_status(out_error, || {
        if out_handle.is_null() {
            return Err((
                ReddbStatus::InvalidArgument,
                "out_handle must not be NULL".to_string(),
            ));
        }
        let path = if path.is_null() {
            None
        } else {
            // SAFETY: guaranteed by this function's C contract.
            let value = unsafe { CStr::from_ptr(path) }.to_str().map_err(|_| {
                (
                    ReddbStatus::InvalidArgument,
                    "database path must be valid UTF-8".to_string(),
                )
            })?;
            (!value.is_empty()).then_some(value)
        };

        let worker = spawn_worker(path.map(str::to_string))
            .map_err(|error| (ReddbStatus::OpenError, error))?;
        let handle = Box::new(ReddbHandle {
            worker: Mutex::new(Some(worker)),
        });
        // SAFETY: `out_handle` was checked above. Ownership transfers to the
        // caller until exactly one `reddb_close`.
        unsafe { ptr::write(out_handle, Box::into_raw(handle)) };
        Ok(())
    }) as c_int
}

/// Execute one JSON-RPC request against an embedded session.
///
/// # Safety
///
/// `handle` must be a live pointer returned by `reddb_open`; `request` must be
/// a NUL-terminated UTF-8 string; `out_response` must be writable.
#[no_mangle]
pub unsafe extern "C" fn reddb_call(
    handle: *mut ReddbHandle,
    request: *const c_char,
    out_response: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> c_int {
    clear_output(out_response);
    ffi_status(out_error, || {
        if handle.is_null() || request.is_null() || out_response.is_null() {
            return Err((
                ReddbStatus::InvalidArgument,
                "handle, request, and out_response must not be NULL".to_string(),
            ));
        }
        // SAFETY: guaranteed by this function's C contract.
        let handle = unsafe { &*handle };
        // SAFETY: guaranteed by this function's C contract.
        let request = unsafe { CStr::from_ptr(request) }.to_str().map_err(|_| {
            (
                ReddbStatus::InvalidArgument,
                "request must be valid UTF-8".to_string(),
            )
        })?;
        let response = call_session(handle, request)?;
        write_string_output(out_response, response)
    }) as c_int
}

/// Invoke one method and return the unwrapped JSON-RPC result value.
///
/// # Safety
///
/// `handle` must be live; `method` and `params_json` must be NUL-terminated
/// UTF-8 strings; `out_result` must be writable. `out_error`, when non-null,
/// must be writable.
#[no_mangle]
pub unsafe extern "C" fn reddb_invoke(
    handle: *mut ReddbHandle,
    method: *const c_char,
    params_json: *const c_char,
    out_result: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> c_int {
    clear_output(out_result);
    ffi_status(out_error, || {
        if handle.is_null() || method.is_null() || params_json.is_null() || out_result.is_null() {
            return Err((
                ReddbStatus::InvalidArgument,
                "handle, method, params_json, and out_result must not be NULL".to_string(),
            ));
        }
        // SAFETY: guaranteed by this function's C contract.
        let handle = unsafe { &*handle };
        // SAFETY: guaranteed by this function's C contract.
        let method = unsafe { CStr::from_ptr(method) }.to_str().map_err(|_| {
            (
                ReddbStatus::InvalidArgument,
                "method must be valid UTF-8".to_string(),
            )
        })?;
        if method.is_empty() {
            return Err((
                ReddbStatus::InvalidArgument,
                "method must not be empty".to_string(),
            ));
        }
        // SAFETY: guaranteed by this function's C contract.
        let params_json = unsafe { CStr::from_ptr(params_json) }
            .to_str()
            .map_err(|_| {
                (
                    ReddbStatus::InvalidArgument,
                    "params_json must be valid UTF-8".to_string(),
                )
            })?;
        let params = json::from_str::<JsonValue>(params_json).map_err(|error| {
            (
                ReddbStatus::InvalidArgument,
                format!("params_json is not valid JSON: {error}"),
            )
        })?;

        let mut request = JsonMap::new();
        request.insert("jsonrpc".to_string(), JsonValue::String("2.0".to_string()));
        request.insert("id".to_string(), JsonValue::Integer(1));
        request.insert("method".to_string(), JsonValue::String(method.to_string()));
        request.insert("params".to_string(), params);
        let response = call_session(handle, &JsonValue::Object(request).to_string_compact())?;
        let envelope = json::from_str::<JsonValue>(&response).map_err(|error| {
            (
                ReddbStatus::InternalError,
                format!("engine returned invalid JSON-RPC: {error}"),
            )
        })?;
        if let Some(error) = envelope.get("error") {
            if !matches!(error, JsonValue::Null) {
                return Err((ReddbStatus::CallError, error.to_string_compact()));
            }
        }
        let result = envelope.get("result").ok_or_else(|| {
            (
                ReddbStatus::InternalError,
                "engine JSON-RPC response has no result or error".to_string(),
            )
        })?;
        write_string_output(out_result, result.to_string_compact())
    }) as c_int
}

/// Checkpoint, close, and free a handle.
///
/// # Safety
///
/// `handle` must be a live pointer returned by `reddb_open`, must not be in use
/// by another thread, and must be passed to this function exactly once.
#[no_mangle]
pub unsafe extern "C" fn reddb_close(
    handle: *mut ReddbHandle,
    out_error: *mut *mut c_char,
) -> c_int {
    ffi_status(out_error, || {
        if handle.is_null() {
            return Err((
                ReddbStatus::InvalidArgument,
                "handle must not be NULL".to_string(),
            ));
        }
        // SAFETY: guaranteed by this function's ownership contract. Rebuild
        // the Box immediately so it is freed on every return path.
        let handle = unsafe { Box::from_raw(handle) };
        handle
            .shutdown()
            .map_err(|error| (ReddbStatus::CallError, error))
    }) as c_int
}

/// Release a response or error string allocated by this library.
///
/// # Safety
///
/// `value` must be NULL or a pointer returned through an output of this ABI,
/// and it must be passed to this function at most once.
#[no_mangle]
pub unsafe extern "C" fn reddb_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: guaranteed by this function's ownership contract.
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn call(handle: *mut ReddbHandle, request: &str) -> String {
        let request = CString::new(request).expect("request has no NUL byte");
        let mut response = ptr::null_mut();
        let mut error = ptr::null_mut();
        // SAFETY: test owns a live handle and valid output slots.
        let status = unsafe { reddb_call(handle, request.as_ptr(), &mut response, &mut error) };
        assert_eq!(status, ReddbStatus::Ok as c_int, "{}", take(error));
        take(response)
    }

    unsafe fn invoke(handle: *mut ReddbHandle, method: &str, params: &str) -> String {
        let method = CString::new(method).expect("method has no NUL byte");
        let params = CString::new(params).expect("params have no NUL byte");
        let mut result = ptr::null_mut();
        let mut error = ptr::null_mut();
        // SAFETY: test owns a live handle and valid input/output pointers.
        let status = unsafe {
            reddb_invoke(
                handle,
                method.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            )
        };
        assert_eq!(status, ReddbStatus::Ok as c_int, "{}", take(error));
        take(result)
    }

    fn take(value: *mut c_char) -> String {
        if value.is_null() {
            return String::new();
        }
        // SAFETY: tests only pass strings allocated by this ABI.
        let result = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: ownership is released exactly once here.
        unsafe { reddb_string_free(value) };
        result
    }

    unsafe fn open(path: Option<&str>) -> *mut ReddbHandle {
        let path = path.map(|value| CString::new(value).expect("path has no NUL byte"));
        let mut handle = ptr::null_mut();
        let mut error = ptr::null_mut();
        // SAFETY: all pointers remain valid for the duration of the call.
        let status = unsafe {
            reddb_open(
                path.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
                &mut handle,
                &mut error,
            )
        };
        assert_eq!(status, ReddbStatus::Ok as c_int, "{}", take(error));
        handle
    }

    unsafe fn close(handle: *mut ReddbHandle) {
        let mut error = ptr::null_mut();
        // SAFETY: test transfers its live handle exactly once.
        let status = unsafe { reddb_close(handle, &mut error) };
        assert_eq!(status, ReddbStatus::Ok as c_int, "{}", take(error));
    }

    #[test]
    fn memory_session_uses_the_canonical_rpc_dispatcher() {
        // SAFETY: this test observes every ownership rule of the C ABI.
        unsafe {
            let handle = open(None);
            let created = call(
                handle,
                r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE ffi_items (id INTEGER, name TEXT)"}}"#,
            );
            assert!(created.contains(r#""id":1"#));
            let inserted = call(
                handle,
                r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"sql":"INSERT INTO ffi_items (id, name) VALUES (1, 'shared')"}}"#,
            );
            assert!(inserted.contains(r#""id":2"#));
            let selected = call(
                handle,
                r#"{"jsonrpc":"2.0","id":3,"method":"query","params":{"sql":"SELECT id, name FROM ffi_items"}}"#,
            );
            assert!(selected.contains("shared"));
            close(handle);
        }
    }

    #[test]
    fn small_stack_foreign_caller_dispatches_to_the_rust_worker() {
        let caller = thread::Builder::new()
            .name("foreign-runtime".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                // SAFETY: this caller observes every ownership rule of the C ABI.
                unsafe {
                    let handle = open(None);
                    let result = invoke(handle, "query", r#"{"sql":"SELECT 1 AS one"}"#);
                    assert!(result.contains(r#""one":1"#), "{result}");
                    close(handle);
                }
            })
            .expect("spawn small-stack caller");
        caller.join().expect("small-stack caller completes");
    }

    #[test]
    fn invoke_returns_the_unwrapped_result_and_structured_error() {
        // SAFETY: this test observes every ownership rule of the C ABI.
        unsafe {
            let handle = open(None);
            let result = invoke(handle, "query", r#"{"sql":"SELECT 1 AS one"}"#);
            assert!(result.contains(r#""one":1"#), "{result}");

            let method = CString::new("query").expect("method has no NUL byte");
            let params =
                CString::new(r#"{"sql":"SELEKT invalid"}"#).expect("params have no NUL byte");
            let mut result = ptr::null_mut();
            let mut error = ptr::null_mut();
            let status = reddb_invoke(
                handle,
                method.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            );
            assert_eq!(status, ReddbStatus::CallError as c_int);
            assert!(result.is_null());
            let error = take(error);
            assert!(error.contains("QUERY_ERROR"), "{error}");
            close(handle);
        }
    }

    #[test]
    fn single_file_survives_close_and_reopen() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("ffi-contract.rdb");
        let path_text = path.to_string_lossy().into_owned();

        // SAFETY: this test observes every ownership rule of the C ABI.
        unsafe {
            let first = open(Some(&path_text));
            call(
                first,
                r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE ffi_persist (id INTEGER, name TEXT)"}}"#,
            );
            call(
                first,
                r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"sql":"INSERT INTO ffi_persist (id, name) VALUES (7, 'persisted')"}}"#,
            );
            close(first);

            let second = open(Some(&path_text));
            let selected = call(
                second,
                r#"{"jsonrpc":"2.0","id":3,"method":"query","params":{"sql":"SELECT id, name FROM ffi_persist"}}"#,
            );
            assert!(selected.contains("persisted"));
            close(second);
        }

        let artifacts = std::fs::read_dir(directory.path())
            .expect("list database directory")
            .map(|entry| entry.expect("directory entry").path())
            .collect::<Vec<_>>();
        assert_eq!(artifacts, [path]);
    }
}
