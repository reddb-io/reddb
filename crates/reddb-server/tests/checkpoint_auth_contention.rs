//! Native artifact preparation must not hold the publication fence needed by login.
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
        if field.name() == "message" && format!("{value:?}") == "native artifact snapshots finished"
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
fn paused_native_preparation_allows_audited_login() {
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
    runtime
        .execute_query("CREATE TABLE prepared_rows (id INT,payload TEXT)")
        .expect("source table");
    runtime
        .execute_query("CREATE UNIQUE INDEX prepared_rows_id ON prepared_rows (id)")
        .expect("source primary index");
    runtime
        .execute_query("CREATE INDEX prepared_rows_payload ON prepared_rows (payload)")
        .expect("source secondary index");
    runtime
        .execute_query("INSERT INTO prepared_rows (id,payload) VALUES (0,'alpha'),(1,'alpha'),(2,'alpha'),(3,'alpha'),(4,'alpha'),(5,'alpha')")
        .expect("six source rows");
    runtime
        .checkpoint()
        .expect("initial native source generation");
    runtime
        .execute_query("UPDATE prepared_rows SET payload='beta'")
        .expect("source requiring preparation");
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
    let (http_finished_tx, http_finished) = mpsc::channel();
    let request = thread::spawn(move || {
        let result = login(server);
        http_finished_tx
            .send(started.elapsed())
            .expect("HTTP completion observer");
        result
    });
    entered
        .recv_timeout(WAIT)
        .expect("credentials accepted; real ledger entered");
    let ledger_entry = started.elapsed();
    let completed_during_preparation = persisted.recv_timeout(Duration::from_secs(5)).is_ok();
    let response_during_preparation = if completed_during_preparation {
        http_finished.recv_timeout(Duration::from_secs(5)).ok()
    } else {
        None
    };
    if completed_during_preparation {
        runtime
            .execute_query("UPDATE prepared_rows SET payload='gamma' WHERE id=3")
            .expect("mutation after native preparation");
    }
    // Always unblock and join the real checkpoint, including on the old failing path.
    release.send(()).expect("release native preparation");
    let (response, worker) = request.join().expect("concurrent login request");
    checkpoint.join().expect("checkpoint worker");
    worker.join().expect("timed HTTP worker");
    assert!(
        completed_during_preparation,
        "native preparation must not block the login ledger until publication"
    );
    let deadline_elapsed = response_during_preparation
        .expect("HTTP login must complete before native preparation is released");
    assert!(
        ledger_entry < Duration::from_secs(10),
        "credential verification must leave time before the 30-second deadline"
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        !runtime
            .db()
            .native_vector_artifact_pages()
            .expect("published caches")
            .iter()
            .any(|page| page.collection == "prepared_rows"),
        "a source changed after preparation must not publish its stale cache"
    );
    let rows = runtime
        .execute_query("SELECT id,payload FROM prepared_rows ORDER BY id")
        .expect("visible rows after concurrent checkpoint")
        .result
        .records;
    assert_eq!(rows.len(), 6);
    for (id, row) in rows.iter().enumerate() {
        assert_eq!(row.get("id").expect("row ID").to_string(), id.to_string());
        assert_eq!(
            row.get("payload").expect("row payload").as_text(),
            Some(if id == 3 { "gamma" } else { "beta" })
        );
    }
    let indexed = runtime
        .execute_query("SELECT id FROM prepared_rows WHERE payload='gamma'")
        .expect("secondary equality while native cache is deferred")
        .result
        .records;
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].get("id").expect("indexed ID").to_string(), "3");

    let server = RedDBServer::new(runtime.clone()).with_auth(auth);
    let (response, worker) = login(server);
    worker.join().expect("recovered HTTP worker");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    runtime.checkpoint().expect("persist completed auth events");
    assert!(
        runtime
            .db()
            .native_vector_artifact_pages()
            .expect("rebuilt caches")
            .iter()
            .any(|page| page.collection == "prepared_rows"),
        "a stable subsequent checkpoint must materialize the deferred cache"
    );
    drop(runtime);
    let recovered =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("cold reopen");
    let indexed = recovered
        .execute_query("SELECT payload FROM prepared_rows WHERE id=3")
        .expect("recovered indexed source")
        .result
        .records;
    assert_eq!(indexed.len(), 1);
    assert_eq!(
        indexed[0]
            .get("payload")
            .expect("recovered payload")
            .as_text(),
        Some("gamma")
    );
    let events = recovered
        .db()
        .store()
        .get_collection(CONTROL_EVENTS_COLLECTION)
        .expect("recovered auth ledger")
        .query_all(|_| true);
    assert_eq!(
        events.len(),
        initial_events + 3,
        "baseline, concurrent and recovered login events must survive"
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
        "login status: 200 -> 200 -> 200; ledger entry: {ledger_entry:?}; \
         concurrent login elapsed: {deadline_elapsed:?}; recovered login events: 3"
    );
}
