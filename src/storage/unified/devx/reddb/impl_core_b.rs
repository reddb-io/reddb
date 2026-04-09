use super::*;

impl RedDB {
    pub fn health(&self) -> HealthReport {
        let mut report = match self.path() {
            Some(path) => storage_file_health(path),
            None => HealthReport::healthy().with_diagnostic("mode", "in-memory"),
        };
        report = report.with_diagnostic("collections", self.collections().len().to_string());
        report = report.with_diagnostic("entities", self.stats().total_entities.to_string());
        let catalog_consistency = self.catalog_consistency_report();
        report = report.with_diagnostic(
            "catalog.declared_indexes",
            catalog_consistency.declared_index_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_indexes",
            catalog_consistency.operational_index_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.declared_graph_projections",
            catalog_consistency
                .declared_graph_projection_count
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_graph_projections",
            catalog_consistency
                .operational_graph_projection_count
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.declared_analytics_jobs",
            catalog_consistency.declared_analytics_job_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_analytics_jobs",
            catalog_consistency
                .operational_analytics_job_count
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_indexes",
            catalog_consistency
                .missing_operational_indexes
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_indexes",
            catalog_consistency
                .undeclared_operational_indexes
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_graph_projections",
            catalog_consistency
                .missing_operational_graph_projections
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_graph_projections",
            catalog_consistency
                .undeclared_operational_graph_projections
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_analytics_jobs",
            catalog_consistency
                .missing_operational_analytics_jobs
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_analytics_jobs",
            catalog_consistency
                .undeclared_operational_analytics_jobs
                .len()
                .to_string(),
        );
        if !catalog_consistency.missing_operational_indexes.is_empty()
            || !catalog_consistency
                .undeclared_operational_indexes
                .is_empty()
            || !catalog_consistency
                .missing_operational_graph_projections
                .is_empty()
            || !catalog_consistency
                .undeclared_operational_graph_projections
                .is_empty()
            || !catalog_consistency
                .missing_operational_analytics_jobs
                .is_empty()
            || !catalog_consistency
                .undeclared_operational_analytics_jobs
                .is_empty()
        {
            report.issue(
                "catalog_consistency",
                "declared and operational catalog state are diverging",
            );
        }
        let catalog_snapshot = self.catalog_model_snapshot();
        let stale_graph_projections = catalog_snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.lifecycle_state == "stale")
            .count();
        let queryable_graph_projections = catalog_snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.queryable)
            .count();
        let graph_projections_requiring_rematerialization = catalog_snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.requires_rematerialization)
            .count();
        let stale_indexes = catalog_snapshot
            .index_statuses
            .iter()
            .filter(|status| status.lifecycle_state == "stale")
            .count();
        let failed_indexes = catalog_snapshot
            .index_statuses
            .iter()
            .filter(|status| status.lifecycle_state == "failed")
            .count();
        let building_indexes = catalog_snapshot
            .index_statuses
            .iter()
            .filter(|status| status.lifecycle_state == "building")
            .count();
        let queryable_indexes = catalog_snapshot
            .index_statuses
            .iter()
            .filter(|status| status.queryable)
            .count();
        let indexes_requiring_rebuild = catalog_snapshot
            .index_statuses
            .iter()
            .filter(|status| status.requires_rebuild)
            .count();
        let graph_projections_rerun_required = catalog_snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.rerun_required)
            .count();
        let stale_analytics_jobs = catalog_snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.lifecycle_state == "stale")
            .count();
        let executable_analytics_jobs = catalog_snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.executable)
            .count();
        let analytics_jobs_requiring_rerun = catalog_snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.requires_rerun)
            .count();
        let analytics_job_dependency_drift = catalog_snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.dependency_in_sync == Some(false))
            .count();
        let collections_requiring_attention = catalog_snapshot
            .collections
            .iter()
            .filter(|collection| collection.attention_required)
            .count();
        let collections_in_sync = catalog_snapshot
            .collections
            .iter()
            .filter(|collection| collection.resources_in_sync)
            .count();
        let attention_summary = crate::catalog::attention_summary(&catalog_snapshot);
        let collection_attention = crate::catalog::collection_attention(&catalog_snapshot);
        let index_attention = crate::catalog::index_attention(&catalog_snapshot);
        let graph_projection_attention =
            crate::catalog::graph_projection_attention(&catalog_snapshot);
        let analytics_job_attention = crate::catalog::analytics_job_attention(&catalog_snapshot);
        report = report.with_diagnostic(
            "catalog.queryable_graph_projections",
            queryable_graph_projections.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.graph_projections_requiring_rematerialization",
            graph_projections_requiring_rematerialization.to_string(),
        );
        report = report.with_diagnostic("catalog.queryable_indexes", queryable_indexes.to_string());
        report = report.with_diagnostic("catalog.stale_indexes", stale_indexes.to_string());
        report = report.with_diagnostic("catalog.failed_indexes", failed_indexes.to_string());
        report = report.with_diagnostic("catalog.building_indexes", building_indexes.to_string());
        report = report.with_diagnostic(
            "catalog.indexes_requiring_rebuild",
            indexes_requiring_rebuild.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.stale_graph_projections",
            stale_graph_projections.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.graph_projections_rerun_required",
            graph_projections_rerun_required.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.stale_analytics_jobs",
            stale_analytics_jobs.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.executable_analytics_jobs",
            executable_analytics_jobs.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.analytics_jobs_requiring_rerun",
            analytics_jobs_requiring_rerun.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.analytics_job_dependency_drift",
            analytics_job_dependency_drift.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.collections_in_sync",
            collections_in_sync.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.collections_requiring_attention",
            collections_requiring_attention.to_string(),
        );
        if let Some(collection) = attention_summary.top_collection.as_ref() {
            report =
                report.with_diagnostic("catalog.top_attention_collection", collection.name.clone());
            report = report.with_diagnostic(
                "catalog.top_attention_collection_score",
                collection.attention_score.to_string(),
            );
            report = report.with_diagnostic(
                "catalog.top_attention_collection_reasons",
                collection.attention_reasons.join(","),
            );
        }
        if let Some(index) = attention_summary.top_index.as_ref() {
            report = report.with_diagnostic("catalog.top_attention_index", index.name.clone());
            report = report.with_diagnostic(
                "catalog.top_attention_index_score",
                index.attention_score.to_string(),
            );
            report = report.with_diagnostic(
                "catalog.top_attention_index_reasons",
                index.attention_reasons.join(","),
            );
        }
        if let Some(projection) = attention_summary.top_graph_projection.as_ref() {
            report = report.with_diagnostic(
                "catalog.top_attention_graph_projection",
                projection.name.clone(),
            );
            report = report.with_diagnostic(
                "catalog.top_attention_graph_projection_score",
                projection.attention_score.to_string(),
            );
            report = report.with_diagnostic(
                "catalog.top_attention_graph_projection_reasons",
                projection.attention_reasons.join(","),
            );
        }
        if let Some(job) = attention_summary.top_analytics_job.as_ref() {
            report = report.with_diagnostic("catalog.top_attention_analytics_job", job.id.clone());
            report = report.with_diagnostic(
                "catalog.top_attention_analytics_job_score",
                job.attention_score.to_string(),
            );
            report = report.with_diagnostic(
                "catalog.top_attention_analytics_job_reasons",
                job.attention_reasons.join(","),
            );
        }
        for collection in &collection_attention {
            let prefix = format!("catalog.collection.{}", collection.name);
            report = report.with_diagnostic(
                format!("{}.resources_in_sync", prefix),
                collection.resources_in_sync.to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.queryable_indexes", prefix),
                collection.queryable_index_count.to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.indexes_requiring_rebuild", prefix),
                collection.indexes_requiring_rebuild_count.to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.queryable_graph_projections", prefix),
                collection.queryable_graph_projection_count.to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.graph_projections_requiring_rematerialization", prefix),
                collection
                    .graph_projections_requiring_rematerialization_count
                    .to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.executable_analytics_jobs", prefix),
                collection.executable_analytics_job_count.to_string(),
            );
            report = report.with_diagnostic(
                format!("{}.analytics_jobs_requiring_rerun", prefix),
                collection.analytics_jobs_requiring_rerun_count.to_string(),
            );
        }
        report = report.with_diagnostic(
            "catalog.index_attention_items",
            index_attention.len().to_string(),
        );
        report = report.with_diagnostic(
            "catalog.graph_projection_attention_items",
            graph_projection_attention.len().to_string(),
        );
        report = report.with_diagnostic(
            "catalog.analytics_job_attention_items",
            analytics_job_attention.len().to_string(),
        );
        if stale_indexes > 0 || failed_indexes > 0 || indexes_requiring_rebuild > 0 {
            report.issue(
                "catalog_indexes",
                "one or more indexes are stale, failed, or require rebuild before they are safely queryable",
            );
        }
        if graph_projections_requiring_rematerialization > 0 {
            report.issue(
                "catalog_graph_projection_readiness",
                "one or more graph projections require rematerialization before they are safely queryable",
            );
        }
        if stale_graph_projections > 0 {
            report.issue(
                "catalog_graph_projections",
                "one or more graph projections are stale and require rematerialization",
            );
        }
        if graph_projections_rerun_required > 0 {
            report.issue(
                "catalog_projection_dependencies",
                "one or more graph projections require dependent analytics jobs to be rerun",
            );
        }
        if stale_analytics_jobs > 0 || analytics_job_dependency_drift > 0 {
            report.issue(
                "catalog_analytics_jobs",
                "one or more analytics jobs are stale or have projection dependency drift",
            );
        }
        if analytics_jobs_requiring_rerun > 0 {
            report.issue(
                "catalog_analytics_job_readiness",
                "one or more analytics jobs require rerun before they should be treated as current",
            );
        }
        if collections_requiring_attention > 0 {
            report.issue(
                "catalog_collection_readiness",
                "one or more collections require operational attention due to drift, rebuild, rematerialization, or analytics rerun state",
            );
        }
        report = report.with_diagnostic(
            "retention.snapshots",
            self.options.snapshot_retention.to_string(),
        );
        report = report.with_diagnostic(
            "retention.exports",
            self.options.export_retention.to_string(),
        );
        if let Some(path) = self.path() {
            let metadata_for_native = self.physical_metadata();
            if let Some(native_state) = self.native_physical_state() {
                let native = native_state.header;
                report =
                    report.with_diagnostic("native_header.sequence", native.sequence.to_string());
                report = report.with_diagnostic(
                    "native_header.format_version",
                    native.format_version.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_root",
                    native.manifest_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_oldest_root",
                    native.manifest_oldest_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.free_set_root",
                    native.free_set_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_page",
                    native.manifest_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_checksum",
                    native.manifest_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_roots_page",
                    native.collection_roots_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_roots_checksum",
                    native.collection_roots_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_root_count",
                    native.collection_root_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.snapshot_count",
                    native.snapshot_count.to_string(),
                );
                report = report
                    .with_diagnostic("native_header.index_count", native.index_count.to_string());
                report = report.with_diagnostic(
                    "native_header.catalog_collection_count",
                    native.catalog_collection_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_total_entities",
                    native.catalog_total_entities.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.export_count",
                    native.export_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.graph_projection_count",
                    native.graph_projection_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.analytics_job_count",
                    native.analytics_job_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_event_count",
                    native.manifest_event_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.registry_page",
                    native.registry_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.registry_checksum",
                    native.registry_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.recovery_page",
                    native.recovery_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.recovery_checksum",
                    native.recovery_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_page",
                    native.catalog_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_checksum",
                    native.catalog_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.metadata_state_page",
                    native.metadata_state_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.metadata_state_checksum",
                    native.metadata_state_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.vector_artifact_page",
                    native.vector_artifact_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.vector_artifact_checksum",
                    native.vector_artifact_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_collection_roots.entries",
                    native_state.collection_roots.len().to_string(),
                );
                if let Some(vector_artifact_pages) = native_state.vector_artifact_pages.as_ref() {
                    report = report.with_diagnostic(
                        "native_vector_artifacts.page_count",
                        vector_artifact_pages.len().to_string(),
                    );
                    match self.inspect_native_vector_artifacts() {
                        Ok(batch) => {
                            report = report.with_diagnostic(
                                "native_vector_artifacts.inspected_count",
                                batch.inspected_count.to_string(),
                            );
                            report = report.with_diagnostic(
                                "native_vector_artifacts.valid_count",
                                batch.valid_count.to_string(),
                            );
                            report = report.with_diagnostic(
                                "native_vector_artifacts.failure_count",
                                batch.failures.len().to_string(),
                            );
                            if !batch.failures.is_empty() {
                                report.issue(
                                    "native_vector_artifacts",
                                    "one or more native vector artifacts could not be deserialized",
                                );
                            }
                        }
                        Err(err) => report.issue("native_vector_artifacts", err),
                    }
                }
                if let Some(metadata) = metadata_for_native.as_ref() {
                    if native_state.collection_roots != metadata.superblock.collection_roots {
                        report.issue(
                            "native_collection_roots",
                            "native collection roots diverge from physical metadata",
                        );
                    }
                }
                if let Some(native_registry) = native_state.registry.as_ref() {
                    report = report.with_diagnostic(
                        "native_registry.collection_count",
                        native_registry.collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.index_count",
                        native_registry.index_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projection_count",
                        native_registry.graph_projection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_job_count",
                        native_registry.analytics_job_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.vector_artifact_count",
                        native_registry.vector_artifact_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.collection_sample_count",
                        native_registry.collection_names.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.collections_complete",
                        native_registry.collections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_collection_count",
                        native_registry.omitted_collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.index_sample_count",
                        native_registry.indexes.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.indexes_complete",
                        native_registry.indexes_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_index_count",
                        native_registry.omitted_index_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projection_sample_count",
                        native_registry.graph_projections.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projections_complete",
                        native_registry.graph_projections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_graph_projection_count",
                        native_registry.omitted_graph_projection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_job_sample_count",
                        native_registry.analytics_jobs.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_jobs_complete",
                        native_registry.analytics_jobs_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.vector_artifacts_complete",
                        native_registry.vector_artifacts_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_analytics_job_count",
                        native_registry.omitted_analytics_job_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_vector_artifact_count",
                        native_registry.omitted_vector_artifact_count.to_string(),
                    );

                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_registry =
                            self.native_registry_summary_from_metadata(metadata);
                        if *native_registry != expected_registry {
                            report.issue(
                                "native_registry",
                                "native registry summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(native_catalog) = native_state.catalog.as_ref() {
                    report = report.with_diagnostic(
                        "native_catalog.collection_count",
                        native_catalog.collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.total_entities",
                        native_catalog.total_entities.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.collection_sample_count",
                        native_catalog.collections.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.collections_complete",
                        native_catalog.collections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.omitted_collection_count",
                        native_catalog.omitted_collection_count.to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_catalog = Self::native_catalog_summary_from_metadata(metadata);
                        if *native_catalog != expected_catalog {
                            report.issue(
                                "native_catalog",
                                "native catalog summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(metadata_state) = native_state.metadata_state.as_ref() {
                    report = report.with_diagnostic(
                        "native_metadata_state.protocol_version",
                        metadata_state.protocol_version.clone(),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.generated_at_unix_ms",
                        metadata_state.generated_at_unix_ms.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.last_loaded_from",
                        metadata_state
                            .last_loaded_from
                            .clone()
                            .unwrap_or_else(|| "null".to_string()),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.last_healed_at_unix_ms",
                        metadata_state
                            .last_healed_at_unix_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "null".to_string()),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_metadata_state =
                            Self::native_metadata_state_summary_from_metadata(metadata);
                        if metadata_state != &expected_metadata_state {
                            report.issue(
                                "native_metadata_state",
                                "native metadata state summary diverges from physical metadata",
                            );
                        }
                    }
                }
                report = report.with_diagnostic(
                    "native_bootstrap.ready",
                    Self::native_state_is_bootstrap_complete(&native_state).to_string(),
                );
                if !Self::native_state_is_bootstrap_complete(&native_state)
                    && metadata_for_native.is_none()
                {
                    report.issue(
                        "native_bootstrap",
                        "native physical publication is partial and cannot rebuild physical metadata without a sidecar",
                    );
                }
                if let Some(native_recovery) = native_state.recovery.as_ref() {
                    report = report.with_diagnostic(
                        "native_recovery.snapshot_count",
                        native_recovery.snapshot_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.export_count",
                        native_recovery.export_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.snapshot_sample_count",
                        native_recovery.snapshots.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.snapshots_complete",
                        native_recovery.snapshots_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.omitted_snapshot_count",
                        native_recovery.omitted_snapshot_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.export_sample_count",
                        native_recovery.exports.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.exports_complete",
                        native_recovery.exports_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.omitted_export_count",
                        native_recovery.omitted_export_count.to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_recovery =
                            Self::native_recovery_summary_from_metadata(metadata);
                        if *native_recovery != expected_recovery {
                            report.issue(
                                "native_recovery",
                                "native recovery summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(native_manifest) = native_state.manifest.as_ref() {
                    report = report.with_diagnostic(
                        "native_manifest.sequence",
                        native_manifest.sequence.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.event_count",
                        native_manifest.event_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.events_complete",
                        native_manifest.events_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.omitted_event_count",
                        native_manifest.omitted_event_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.sample_count",
                        native_manifest.recent_events.len().to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        if native_manifest.event_count != metadata.manifest_events.len() as u32 {
                            report.issue(
                                "native_manifest",
                                "native manifest summary diverges from physical metadata",
                            );
                        }
                    }
                }
            } else if self.store.physical_file_header().is_some() {
                report.issue(
                    "native_state",
                    "native physical state is not fully readable from the paged file",
                );
            }
            let metadata_path = PhysicalMetadataFile::metadata_path_for(path);
            let metadata_binary_path = PhysicalMetadataFile::metadata_binary_path_for(path);
            report = report.with_diagnostic("metadata.path", metadata_path.display().to_string());
            report = report.with_diagnostic(
                "metadata.binary_path",
                metadata_binary_path.display().to_string(),
            );
            report = report.with_diagnostic("metadata.exists", metadata_path.exists().to_string());
            report = report.with_diagnostic(
                "metadata.binary_exists",
                metadata_binary_path.exists().to_string(),
            );
            if let Some(preference) = self.physical_metadata_preference() {
                report = report.with_diagnostic("metadata.preference", preference);
            }
            if let Ok((metadata, source)) =
                PhysicalMetadataFile::load_for_data_path_with_source(path)
            {
                let journal_count = PhysicalMetadataFile::journal_paths_for_data_path(path)
                    .map(|paths| paths.len())
                    .unwrap_or(0);
                report = report.with_diagnostic("metadata.loaded_from", source.as_str());
                report =
                    report.with_diagnostic("metadata.journal_entries", journal_count.to_string());
                report = report.with_diagnostic(
                    "metadata.last_loaded_from",
                    metadata
                        .last_loaded_from
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                );
                report = report.with_diagnostic(
                    "metadata.last_healed_at_unix_ms",
                    metadata
                        .last_healed_at_unix_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "null".to_string()),
                );
                report = report.with_diagnostic(
                    "metadata.sequence",
                    metadata.superblock.sequence.to_string(),
                );
                report = report
                    .with_diagnostic("metadata.snapshots", metadata.snapshots.len().to_string());
                report =
                    report.with_diagnostic("metadata.indexes", metadata.indexes.len().to_string());
                report =
                    report.with_diagnostic("metadata.exports", metadata.exports.len().to_string());
                if let Some(native) = self.store.physical_file_header() {
                    let inspection =
                        Self::inspect_native_header_against_metadata(native, &metadata);
                    report = report.with_diagnostic(
                        "native_header.matches_metadata",
                        inspection.consistent.to_string(),
                    );
                    let policy = Self::repair_policy_for_inspection(&inspection);
                    report = report.with_diagnostic(
                        "native_header.repair_policy",
                        match policy {
                            NativeHeaderRepairPolicy::InSync => "in_sync",
                            NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                                "repair_native_from_metadata"
                            }
                            NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                                "native_ahead_of_metadata"
                            }
                        },
                    );
                    if !inspection.consistent {
                        match policy {
                            NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                                report.issue(
                                    "native_header",
                                    format!(
                                        "native header diverges from physical metadata on {} field(s); repairable from metadata",
                                        inspection.mismatches.len()
                                    ),
                                );
                            }
                            NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                                report.issue(
                                    "native_header",
                                    format!(
                                        "native header diverges from physical metadata on {} field(s); native header appears ahead of metadata",
                                        inspection.mismatches.len()
                                    ),
                                );
                            }
                            NativeHeaderRepairPolicy::InSync => {}
                        }
                        for mismatch in inspection.mismatches {
                            report = report.with_diagnostic(
                                format!("native_header.mismatch.{}", mismatch.field),
                                format!(
                                    "native={} expected={}",
                                    mismatch.native, mismatch.expected
                                ),
                            );
                        }
                    }
                }
                report = report.with_diagnostic(
                    "metadata.collection_roots",
                    metadata.superblock.collection_roots.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.manifest_events",
                    metadata.manifest_events.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.graph_projections",
                    metadata.graph_projections.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.analytics_jobs",
                    metadata.analytics_jobs.len().to_string(),
                );
            } else if self.options.mode == StorageMode::Persistent {
                report.issue(
                    "metadata",
                    "physical metadata sidecar is missing or unreadable",
                );
            }
        }
        report.with_diagnostic("paged_mode", self.paged_mode.to_string())
    }
}
