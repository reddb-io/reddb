use std::collections::BTreeMap;

use crate::application::ports::RuntimeNativePort;
use crate::health::HealthReport;
use crate::physical::{ExportDescriptor, ManifestEvent, PhysicalMetadataFile, SnapshotDescriptor};
use crate::storage::engine::PhysicalFileHeader;
use crate::storage::unified::devx::{
    NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection, PhysicalAuthorityStatus,
};
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeManifestSummary, NativeMetadataStateSummary, NativePhysicalState,
    NativeRecoverySummary, NativeRegistrySummary, NativeVectorArtifactPageSummary,
};
use crate::RedDBResult;

#[derive(Debug, Clone)]
pub struct InspectNativeArtifactInput {
    pub collection: String,
    pub artifact_kind: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeReadiness {
    pub query: bool,
    pub query_serverless: bool,
    pub write: bool,
    pub write_serverless: bool,
    pub repair: bool,
    pub repair_serverless: bool,
}

pub struct NativeUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeNativePort + ?Sized> NativeUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        self.runtime.collection_roots()
    }

    pub fn health(&self) -> HealthReport {
        self.runtime.health_report()
    }

    pub fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>> {
        self.runtime.snapshots()
    }

    pub fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>> {
        self.runtime.exports()
    }

    pub fn physical_metadata(&self) -> RedDBResult<PhysicalMetadataFile> {
        self.runtime.physical_metadata()
    }

    pub fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>> {
        self.runtime
            .manifest_events_filtered(collection, kind, since_snapshot)
    }

    pub fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor> {
        self.runtime.create_snapshot()
    }

    pub fn create_export(&self, name: String) -> RedDBResult<ExportDescriptor> {
        self.runtime.create_export(name)
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        self.runtime.checkpoint()
    }

    pub fn apply_retention_policy(&self) -> RedDBResult<()> {
        self.runtime.apply_retention_policy()
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.runtime.run_maintenance()
    }

    pub fn native_header(&self) -> RedDBResult<PhysicalFileHeader> {
        self.runtime.native_header()
    }

    pub fn native_collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        self.runtime.native_collection_roots()
    }

    pub fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary> {
        self.runtime.native_manifest_summary()
    }

    pub fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary> {
        self.runtime.native_registry_summary()
    }

    pub fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary> {
        self.runtime.native_recovery_summary()
    }

    pub fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary> {
        self.runtime.native_catalog_summary()
    }

    pub fn native_physical_state(&self) -> RedDBResult<NativePhysicalState> {
        self.runtime.native_physical_state()
    }

    pub fn native_vector_artifact_pages(
        &self,
    ) -> RedDBResult<Vec<NativeVectorArtifactPageSummary>> {
        self.runtime.native_vector_artifact_pages()
    }

    pub fn inspect_vector_artifact(
        &self,
        input: InspectNativeArtifactInput,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        self.runtime
            .inspect_native_vector_artifact(&input.collection, input.artifact_kind.as_deref())
    }

    pub fn warmup_vector_artifact(
        &self,
        input: InspectNativeArtifactInput,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        self.runtime
            .warmup_native_vector_artifact(&input.collection, input.artifact_kind.as_deref())
    }

    pub fn inspect_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        self.runtime.inspect_native_vector_artifacts()
    }

    pub fn warmup_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        self.runtime.warmup_native_vector_artifacts()
    }

    pub fn native_header_repair_policy(&self) -> RedDBResult<String> {
        self.runtime.native_header_repair_policy()
    }

    pub fn repair_native_header_from_metadata(&self) -> RedDBResult<String> {
        self.runtime.repair_native_header_from_metadata()
    }

    pub fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool> {
        self.runtime.rebuild_physical_metadata_from_native_state()
    }

    pub fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool> {
        self.runtime.repair_native_physical_state_from_metadata()
    }

    pub fn native_metadata_state_summary(&self) -> RedDBResult<NativeMetadataStateSummary> {
        self.runtime.native_metadata_state_summary()
    }

    pub fn physical_authority_status(&self) -> PhysicalAuthorityStatus {
        self.runtime.physical_authority_status()
    }

    pub fn readiness(&self) -> RuntimeReadiness {
        RuntimeReadiness {
            query: self.runtime.readiness_for_query(),
            query_serverless: self.runtime.readiness_for_query_serverless(),
            write: self.runtime.readiness_for_write(),
            write_serverless: self.runtime.readiness_for_write_serverless(),
            repair: self.runtime.readiness_for_repair(),
            repair_serverless: self.runtime.readiness_for_repair_serverless(),
        }
    }
}
