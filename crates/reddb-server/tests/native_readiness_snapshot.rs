//! Aggregate readiness must reuse the health observation supplied by its caller.
use reddb_server::application::{NativeUseCases, RuntimeReadiness};
use reddb_server::{HealthReport, RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn scalar_readiness(runtime: &RedDBRuntime) -> RuntimeReadiness {
    RuntimeReadiness {
        query: runtime.readiness_for_query(),
        query_serverless: runtime.readiness_for_query_serverless(),
        write: runtime.readiness_for_write(),
        write_serverless: runtime.readiness_for_write_serverless(),
        repair: runtime.readiness_for_repair(),
        repair_serverless: runtime.readiness_for_repair_serverless(),
    }
}

#[test]
fn aggregate_readiness_preserves_scalar_decisions_across_profiles() {
    let directory = tempfile::tempdir().expect("temporary stores");
    let options = [
        RedDBOptions::in_memory(),
        RedDBOptions::persistent(directory.path().join("embedded.rdb"))
            .with_storage_profile(StorageDeployPreset::Embedded.selection())
            .expect("embedded profile"),
        RedDBOptions::persistent(directory.path().join("serverless.rdb"))
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("serverless profile"),
    ];
    for options in options {
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("CREATE TABLE readiness_probe (value INT)")
            .expect("collection");
        let expected = scalar_readiness(&runtime);
        assert_eq!(NativeUseCases::new(&runtime).readiness(), expected);
    }
}

#[test]
fn supplied_unhealthy_report_is_not_replaced_by_another_health_check() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    assert!(
        runtime.readiness_for_write(),
        "healthy baseline permits writes"
    );
    let native = NativeUseCases::new(&runtime);
    let readiness = native.readiness_from_health(&HealthReport::unhealthy("injected fault"));
    assert_eq!(
        readiness,
        RuntimeReadiness {
            query: false,
            query_serverless: false,
            write: false,
            write_serverless: false,
            repair: false,
            repair_serverless: false,
        },
        "readiness must use the supplied health state rather than recomputing it"
    );
}
