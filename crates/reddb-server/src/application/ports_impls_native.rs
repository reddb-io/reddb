use super::*;
use crate::RedDBError;
impl RuntimeNativePort for RedDBRuntime {
    fn health_report(&self) -> crate::health::HealthReport {
        self.health()
    }

    fn collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        RedDBRuntime::collection_roots(self)
    }

    fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>> {
        RedDBRuntime::snapshots(self)
    }

    fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>> {
        RedDBRuntime::exports(self)
    }

    fn physical_metadata(&self) -> RedDBResult<PhysicalMetadataFile> {
        self.db()
            .physical_metadata()
            .ok_or_else(|| crate::RedDBError::NotFound("physical metadata".to_string()))
    }

    fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>> {
        RedDBRuntime::manifest_events_filtered(self, collection, kind, since_snapshot)
    }

    fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor> {
        RedDBRuntime::create_snapshot(self)
    }

    fn create_export(&self, name: String) -> RedDBResult<ExportDescriptor> {
        RedDBRuntime::create_export(self, name)
    }

    fn checkpoint(&self) -> RedDBResult<()> {
        RedDBRuntime::checkpoint(self)
    }

    fn apply_retention_policy(&self) -> RedDBResult<()> {
        RedDBRuntime::apply_retention_policy(self)
    }

    fn run_maintenance(&self) -> RedDBResult<()> {
        RedDBRuntime::run_maintenance(self)
    }

    fn native_header(&self) -> RedDBResult<PhysicalFileHeader> {
        RedDBRuntime::native_header(self)
    }

    fn native_collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        RedDBRuntime::native_collection_roots(self)
    }

    fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary> {
        RedDBRuntime::native_manifest_summary(self)
    }

    fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary> {
        RedDBRuntime::native_registry_summary(self)
    }

    fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary> {
        RedDBRuntime::native_recovery_summary(self)
    }

    fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary> {
        RedDBRuntime::native_catalog_summary(self)
    }

    fn native_physical_state(&self) -> RedDBResult<NativePhysicalState> {
        RedDBRuntime::native_physical_state(self)
    }

    fn native_vector_artifact_pages(&self) -> RedDBResult<Vec<NativeVectorArtifactPageSummary>> {
        RedDBRuntime::native_vector_artifact_pages(self)
    }

    fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        RedDBRuntime::inspect_native_vector_artifact(self, collection, artifact_kind)
    }

    fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        RedDBRuntime::warmup_native_vector_artifact(self, collection, artifact_kind)
    }

    fn inspect_native_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        RedDBRuntime::inspect_native_vector_artifacts(self)
    }

    fn warmup_native_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        RedDBRuntime::warmup_native_vector_artifacts(self)
    }

    fn native_header_repair_policy(&self) -> RedDBResult<String> {
        RedDBRuntime::native_header_repair_policy(self)
    }

    fn repair_native_header_from_metadata(&self) -> RedDBResult<String> {
        RedDBRuntime::repair_native_header_from_metadata(self)
    }

    fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool> {
        RedDBRuntime::rebuild_physical_metadata_from_native_state(self)
    }

    fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool> {
        RedDBRuntime::repair_native_physical_state_from_metadata(self)
    }

    fn native_metadata_state_summary(&self) -> RedDBResult<NativeMetadataStateSummary> {
        RedDBRuntime::native_metadata_state_summary(self)
    }

    fn physical_authority_status(&self) -> PhysicalAuthorityStatus {
        RedDBRuntime::physical_authority_status(self)
    }

    fn readiness_for_query(&self) -> bool {
        RedDBRuntime::readiness_for_query(self)
    }

    fn readiness_for_query_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_query_serverless(self)
    }

    fn readiness_for_write(&self) -> bool {
        RedDBRuntime::readiness_for_write(self)
    }

    fn readiness_for_write_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_write_serverless(self)
    }

    fn readiness_for_repair(&self) -> bool {
        RedDBRuntime::readiness_for_repair(self)
    }

    fn readiness_for_repair_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_repair_serverless(self)
    }
}
