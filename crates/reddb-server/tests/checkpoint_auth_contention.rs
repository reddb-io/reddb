//! Characterize the login deadline while native artifact preparation holds publication.
//! The real checkpoint sleeps in a thread-local tracing subscriber: no large corpus,
//! HTTP queue or busy CPU is required to reproduce the availability failure.
use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reddb_server::auth::{AuthConfig, AuthStore, Role};
use reddb_server::runtime::control_events::{
    ControlEvent, ControlEventConfig, ControlEventCtx, ControlEventError, ControlEventLedger,
    EventId, RuntimeLedger, CONTROL_EVENTS_COLLECTION,
};
use reddb_server::server::RedDBServer;
use reddb_server::storage::EntityData;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const WAIT: Duration = Duration::from_secs(60);

struct PreparationMessage(bool);

impl Visit for PreparationMessage {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && format!("{value:?}") == "native artifact snapshots started"
        {
            self.0 = true;
        }
    }
}

struct PausePreparation {
    reached: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl<S: Subscriber> Layer<S> for PausePreparation {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "reddb::checkpoint" {
            return;
        }
        let mut message = PreparationMessage(false);
        event.record(&mut message);
        if message.0 {
            self.reached.send(()).expect("checkpoint observer");
            self.release
                .lock()
                .expect("pause receiver")
                .recv_timeout(WAIT)
                .expect("release checkpoint preparation");
        }
    }
}

struct ObservedLedger {
    inner: RuntimeLedger,
    entered: mpsc::Sender<()>,
    persisted: mpsc::Sender<()>,
}

impl ControlEventLedger for ObservedLedger {
    fn emit(
        &self,
        ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        self.entered.send(()).expect("ledger entry observer");
        let result = self.inner.emit(ctx, event);
        self.persisted.send(()).expect("ledger exit observer");
        result
    }
}

fn login(server: RedDBServer) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("isolated listener");
    let address = listener.local_addr().expect("listener address");
    let worker = thread::spawn(move || server.serve_one_on(listener).expect("HTTP server"));
    let mut stream = TcpStream::connect(address).expect("HTTP client");
    stream.set_read_timeout(Some(WAIT)).expect("read deadline");
    let body = r#"{"username":"alice","password":"local-fixture-password"}"#;
    write!(
        stream,
        "POST /auth/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("login request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("login response");
    (response, worker)
}

#[test]
fn paused_native_preparation_exhausts_audited_login_deadline() {
    let directory = tempfile::tempdir().expect("isolated database");
    let path = directory.path().join("auth-contention.rdb");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile"),
    )
    .expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("explicit maintenance only");
    let auth = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        ..Default::default()
    }));
    auth.create_user("alice", "local-fixture-password", Role::Admin)
        .expect("fixture user");
    let (entered_tx, entered) = mpsc::channel();
    let (persisted_tx, persisted) = mpsc::channel();
    auth.configure_control_events(
        Arc::new(ObservedLedger {
            inner: RuntimeLedger::new(runtime.db().store()),
            entered: entered_tx,
            persisted: persisted_tx,
        }),
        ControlEventConfig {
            compliance_mode: true,
        },
    );
    let initial_events = runtime
        .db()
        .store()
        .get_collection(CONTROL_EVENTS_COLLECTION)
        .expect("initial ledger")
        .query_all(|_| true)
        .len();
    let server = RedDBServer::new(runtime.clone()).with_auth(Arc::clone(&auth));
    assert_eq!(server.handler_timeout(), Duration::from_secs(30));
    let (response, worker) = login(server.clone());
    worker.join().expect("baseline HTTP worker");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    entered.recv_timeout(WAIT).expect("baseline ledger entry");
    persisted.recv_timeout(WAIT).expect("baseline ledger exit");

    let (reached_tx, reached) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let checkpoint = {
        let runtime = runtime.clone();
        thread::spawn(move || {
            let subscriber = tracing_subscriber::registry().with(PausePreparation {
                reached: reached_tx,
                release: Mutex::new(release_rx),
            });
            tracing::subscriber::with_default(subscriber, || {
                runtime.checkpoint().expect("paused real checkpoint");
            });
        })
    };
    reached
        .recv_timeout(WAIT)
        .expect("native preparation reached");
    let started = Instant::now();
    let request = thread::spawn(move || login(server));
    entered
        .recv_timeout(WAIT)
        .expect("credentials accepted; real ledger entered");
    let ledger_entry = started.elapsed();
    let blocked = matches!(
        persisted.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    let (response, worker) = request.join().expect("timed login request");
    let deadline_elapsed = started.elapsed();
    // Always unblock and join the real checkpoint before checking results.
    release.send(()).expect("release native preparation");
    checkpoint.join().expect("checkpoint worker");
    worker.join().expect("timed HTTP worker");
    persisted
        .recv_timeout(WAIT)
        .expect("ledger completes after release");
    assert!(
        blocked,
        "ledger persisted despite the held publication fence"
    );
    assert!(
        ledger_entry < Duration::from_secs(10),
        "credential verification must leave time before the 30-second deadline"
    );
    assert!(response.starts_with("HTTP/1.1 503"), "{response}");
    assert!(response.contains("handler deadline exceeded"), "{response}");

    let server = RedDBServer::new(runtime.clone()).with_auth(auth);
    let (response, worker) = login(server);
    worker.join().expect("recovered HTTP worker");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    runtime.checkpoint().expect("persist completed auth events");
    drop(runtime);
    let recovered =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("cold reopen");
    let events = recovered
        .db()
        .store()
        .get_collection(CONTROL_EVENTS_COLLECTION)
        .expect("recovered auth ledger")
        .query_all(|_| true);
    assert_eq!(
        events.len(),
        initial_events + 3,
        "baseline, timed-out and recovered login events must survive"
    );
    let mut login_ids = std::collections::HashSet::new();
    for event in &events {
        let EntityData::Row(row) = &event.data else {
            panic!("control event must be a row");
        };
        let fields = row.named.as_ref().expect("event fields");
        if fields["kind"].as_text() == Some("auth.login") {
            assert_eq!(fields["outcome"].as_text(), Some("allowed"));
            assert_eq!(fields["actor_kind"].as_text(), Some("user"));
            assert_eq!(fields["actor_user_id"].as_text(), Some("alice"));
            assert!(login_ids.insert(fields["id"].as_text().expect("event ID")));
        }
    }
    assert_eq!(login_ids.len(), 3, "three distinct, complete login events");
    println!(
        "login status: 200 -> 503 -> 200; ledger entry: {ledger_entry:?}; \
         HTTP deadline elapsed: {deadline_elapsed:?}; recovered login events: 3"
    );
}
