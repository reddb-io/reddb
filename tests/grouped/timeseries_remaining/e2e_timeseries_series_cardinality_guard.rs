use reddb::application::{
    CreateTimeSeriesInput, CreateTimeSeriesPointInput, EntityUseCases, SchemaUseCases,
};
use reddb::runtime::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn create_timeseries(rt: &RedDBRuntime, name: &str) {
    SchemaUseCases::new(rt)
        .create_timeseries(CreateTimeSeriesInput {
            name: name.to_string(),
            retention_ms: Some(86_400_000),
            chunk_size: None,
            downsample_policies: Vec::new(),
            if_not_exists: false,
        })
        .expect("create timeseries");
}

fn write_point(rt: &RedDBRuntime, collection: &str, host: &str, timestamp_ns: u64) {
    EntityUseCases::new(rt)
        .create_timeseries_point(CreateTimeSeriesPointInput {
            collection: collection.to_string(),
            metric: "cpu.usage".to_string(),
            value: 1.0,
            timestamp_ns: Some(timestamp_ns),
            tags: vec![("host".to_string(), host.to_string())],
            metadata: Vec::new(),
        })
        .expect("create timeseries point");
}

#[test]
fn per_collection_series_ceiling_rejects_only_new_series_and_can_be_raised() {
    let rt = runtime();
    create_timeseries(&rt, "metrics");
    rt.execute_query("SET CONFIG storage.time_series.collections.metrics.max_series = 2")
        .expect("set series ceiling");

    write_point(&rt, "metrics", "a", 1);
    write_point(&rt, "metrics", "b", 2);
    write_point(&rt, "metrics", "a", 3);

    let err = EntityUseCases::new(&rt)
        .create_timeseries_point(CreateTimeSeriesPointInput {
            collection: "metrics".to_string(),
            metric: "cpu.usage".to_string(),
            value: 1.0,
            timestamp_ns: Some(4),
            tags: vec![("host".to_string(), "c".to_string())],
            metadata: Vec::new(),
        })
        .expect_err("new series beyond the ceiling must fail");
    let message = err.to_string();
    assert!(
        message.contains("metrics") && message.contains('2'),
        "error must name collection and ceiling, got {message}"
    );

    rt.execute_query("SET CONFIG storage.time_series.collections.metrics.max_series = 3")
        .expect("raise series ceiling");
    write_point(&rt, "metrics", "c", 5);
}
