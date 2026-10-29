#[test]
fn cold_ledger_first_event_reuses_proven_native_text() {
    use crate::runtime::control_events::{
        ActorRef, ControlEvent, ControlEventCtx, ControlEventLedger, EventKind, Outcome,
        RuntimeLedger, CONTROL_EVENTS_COLLECTION,
    };
    use super::impl_fulltext_delta::APPLIED_ROWS;

    let directory = tempfile::tempdir().expect("ledger fixture");
    let options = paged_options(&directory.path().join("ledger.rdb"));
    let emit = |runtime: &crate::RedDBRuntime| {
        let ledger = RuntimeLedger::new(runtime.db().store());
        ledger.emit(
            &ControlEventCtx {
                actor: ActorRef::System("fixture"),
                scope: None,
                request_id: None,
                trace_id: None,
            },
            ControlEvent {
                kind: EventKind::AuthLogin,
                outcome: Outcome::Allowed,
                action: "auth.login".into(),
                resource: None,
                reason: None,
                matched_policy_id: None,
                fields: HashMap::new(),
            },
        ).expect("durable login event")
    };
    let canonical_frame = |runtime: &crate::RedDBRuntime| {
        let db = runtime.db();
        let header = db.store.physical_file_header().expect("published header");
        let (_, bytes) = db.store.read_native_vector_artifact_blob(
            header.vector_artifact_page, CONTROL_EVENTS_COLLECTION, Some("text.fulltext"),
        ).expect("read ledger frame").expect("ledger text artifact");
        let expected = db.native_vector_artifact_records(&[CONTROL_EVENTS_COLLECTION.to_owned()]);
        assert_eq!(expected.len(), 1);
        assert_eq!(bytes, expected[0].1, "persisted frame equals full derivation");
        bytes.len()
    };
    let runtime = crate::RedDBRuntime::with_options(options.clone()).expect("seed runtime");
    for _ in 0..6 { emit(&runtime); }
    runtime.checkpoint().expect("six-event source");
    APPLIED_ROWS.with(|counts| counts.borrow_mut().clear());
    emit(&runtime);
    runtime.checkpoint().expect("warm one-event checkpoint");
    let warm_delta = APPLIED_ROWS.with(|counts| counts.borrow().get(CONTROL_EVENTS_COLLECTION).copied());
    let warm_bytes = canonical_frame(&runtime);
    assert_eq!(warm_delta, Some(1), "warm control uses one-row delta");
    drop(runtime);

    let opened = std::time::Instant::now();
    let runtime = crate::RedDBRuntime::with_options(options.clone()).expect("cold reopen");
    let open_us = opened.elapsed().as_micros();
    APPLIED_ROWS.with(|counts| counts.borrow_mut().clear());
    let acknowledged = emit(&runtime);
    let published = std::time::Instant::now();
    runtime.checkpoint().expect("cold first-event checkpoint");
    let first_publication_us = published.elapsed().as_micros();
    let cold_delta = APPLIED_ROWS.with(|counts| counts.borrow().get(CONTROL_EVENTS_COLLECTION).copied());
    let cold_bytes = canonical_frame(&runtime);
    drop(runtime);
    let recovered = crate::RedDBRuntime::with_options(options).expect("event recovery");
    let events = recovered.db().store().get_collection(CONTROL_EVENTS_COLLECTION)
        .expect("ledger").query_all(|_| true);
    assert_eq!(events.len(), 8, "all eight acknowledged events recover");
    assert!(events.iter().any(|event| match &event.data {
        EntityData::Row(row) => row.named.as_ref().and_then(|r| r.get("id"))
            .and_then(Value::as_text) == Some(acknowledged.0.as_str()),
        _ => false,
    }), "cold first event recovers by its acknowledged ID");
    println!("warm_delta={warm_delta:?} cold_delta={cold_delta:?} warm_bytes={warm_bytes} cold_bytes={cold_bytes} open_us={open_us} first_publication_us={first_publication_us} content_and_recovery=true");
    assert_eq!(cold_delta, Some(1), "a single event before cold first publication must use one-row delta");
}
