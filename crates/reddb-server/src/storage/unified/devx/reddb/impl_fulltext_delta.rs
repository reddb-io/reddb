use super::*;

#[cfg(test)]
std::thread_local! {
    pub(super) static APPLIED_ROWS: std::cell::RefCell<HashMap<String, usize>> =
        std::cell::RefCell::new(HashMap::new());
}

impl RedDB {
    /// Avoid tokenizing an entire row collection for a bounded journal delta.
    /// Unknown journals, models, references or document coverage use the full
    /// builder. The caller publishes the new revision proof only after sync.
    pub(super) fn prepare_native_fulltext_delta(
        &self,
        collection: &str,
        manager: &Arc<super::super::super::manager::SegmentManager>,
        previous: &NativeArtifactVersion,
        current: &NativeArtifactVersion,
        existing: &[NativeVectorArtifactPageSummary],
    ) -> Option<(NativeVectorArtifactSummary, Vec<u8>, bool)> {
        if !previous.fulltext_document_ids_complete
            || !previous.manager.ptr_eq(&current.manager)
            || previous.summaries.len() != 1
            || previous.pages.len() != 1
            || previous.summaries[0].artifact_kind != "text.fulltext"
            || !existing.contains(&previous.pages[0])
            || existing
                .iter()
                .filter(|page| page.collection == collection)
                .count()
                != 1
        {
            return None;
        }
        let budget = crate::storage::memory_budget::resolve_for_boot(
            self.options.storage_profile.deploy_profile,
            self.options.memory_budget_bytes,
        )
        .ok()?;
        // Decoded terms/postings and the newly encoded frame coexist briefly.
        // Bound this optimization; large indexes retain the existing builder.
        let limit = (budget.resolved_bytes / 8).min(64 * 1024 * 1024);
        let summary = &previous.summaries[0];
        if summary.serialized_bytes > limit {
            return None;
        }
        let ids = manager.checkpoint_mutations_since(&previous.segments)?;
        if ids.len() > crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX {
            return None;
        }
        let mut changes = BTreeMap::new();
        let mut changed_bytes = 0_u64;
        let changed_limit = (limit / 8).min(8 * 1024 * 1024);
        for id in ids {
            let text = match manager.get_with(id, |entity| {
                let EntityData::Row(row) = &entity.data else {
                    return None;
                };
                // A changed JSON/blob column can introduce a document-path
                // artifact even when the previous collection had only text.
                if row
                    .columns
                    .iter()
                    .chain(row.named.iter().flat_map(|fields| fields.values()))
                    .any(|value| matches!(value, Value::Json(_) | Value::Blob(_)))
                {
                    return None;
                }
                // Reject oversized rows before cloning their variable payload.
                // The estimate is deliberately conservative for this fast path.
                (reddb_engine::storage::memory_size::entity_bytes(entity) as u64
                    <= changed_limit.saturating_sub(changed_bytes))
                .then(|| Self::native_fulltext_text_for_entity(&entity.data))
            }) {
                Some(Some(text)) => {
                    changed_bytes = changed_bytes.checked_add(text.len() as u64)?;
                    if changed_bytes > changed_limit {
                        return None;
                    }
                    Some(text)
                }
                Some(None) => return None,
                None => None,
            };
            changes.insert(id.raw(), text);
        }
        let bytes = {
            // A concurrent publisher can retire/reuse the old chain. Validate
            // its identity and copy its frame under the fence; decode, delta
            // application and serialization run after releasing the fence.
            let _publication = self.store.vault_publication_lock.lock();
            if !self
                .current_native_artifact_pages()
                .ok()?
                .contains(&previous.pages[0])
            {
                return None;
            }
            self.store
                .read_native_artifact_payload(&previous.pages[0])
                .ok()?
        };
        if bytes.len() as u64 != summary.serialized_bytes
            || crate::storage::engine::crc32(&bytes) as u64 != summary.checksum
        {
            return None;
        }
        let mut frame = reddb_file::decode_native_fulltext_frame(&bytes).ok()?;
        drop(bytes);
        if frame.collection != collection || frame.doc_count != summary.dimension {
            return None;
        }
        let complete = super::native_fulltext_delta::apply_delta(&mut frame, &changes, true)?;
        // The full builder omits an artifact with no nonempty documents.
        if frame.doc_count == 0 || manager.checkpoint_revisions() != current.segments {
            return None;
        }
        let term_count = u32::try_from(frame.terms.len()).ok()?;
        let posting_count = frame
            .terms
            .iter()
            .map(|term| term.postings.len() as u64)
            .sum();
        let bytes = reddb_file::encode_native_fulltext_frame(&frame);
        let summary = NativeVectorArtifactSummary {
            collection: collection.to_owned(),
            artifact_kind: "text.fulltext".to_owned(),
            vector_count: posting_count,
            dimension: frame.doc_count,
            max_layer: term_count,
            serialized_bytes: bytes.len() as u64,
            checksum: crate::storage::engine::crc32(&bytes) as u64,
        };
        #[cfg(test)]
        APPLIED_ROWS.with(|counts| {
            *counts
                .borrow_mut()
                .entry(collection.to_owned())
                .or_default() += changes.len();
        });
        Some((summary, bytes, complete))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_rejects_unproved_sources_references_revisions_and_size() {
        let directory = tempfile::tempdir().expect("temporary store");
        let options = RedDBOptions::persistent(directory.path().join("guards.rdb"))
            .with_storage_profile(crate::storage::StorageDeployPreset::Serverless.selection())
            .expect("paged profile");
        let runtime = crate::RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("CREATE TABLE guarded_rows (payload TEXT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO guarded_rows (payload) VALUES ('alpha beta')")
            .expect("seed");
        runtime.checkpoint().expect("baseline");
        let db = runtime.db();
        let manager = db.store.get_collection("guarded_rows").expect("manager");
        let previous = db.native_artifact_versions.lock()["guarded_rows"].clone();
        let header = db.store.physical_file_header().expect("header");
        let existing = db
            .store
            .read_native_vector_artifact_store(header.vector_artifact_page)
            .expect("references");
        runtime
            .execute_query("INSERT INTO guarded_rows (payload) VALUES ('gamma delta')")
            .expect("append");
        let mut current = previous.clone();
        current.segments = manager.checkpoint_revisions();
        let delta = |previous: &NativeArtifactVersion,
                     current: &NativeArtifactVersion,
                     existing: &[NativeVectorArtifactPageSummary]| {
            db.prepare_native_fulltext_delta("guarded_rows", &manager, previous, current, existing)
        };
        assert!(delta(&previous, &current, &existing).is_some());

        let mut unproved = previous.clone();
        unproved.fulltext_document_ids_complete = false;
        assert!(delta(&unproved, &current, &existing).is_none());
        unproved = previous.clone();
        unproved.manager = std::sync::Weak::new();
        assert!(delta(&unproved, &current, &existing).is_none());
        unproved = previous.clone();
        unproved.segments.push((u64::MAX, 0));
        assert!(delta(&unproved, &current, &existing).is_none());
        unproved = previous.clone();
        unproved.summaries[0].serialized_bytes = u64::MAX;
        assert!(delta(&unproved, &current, &existing).is_none());
        unproved = previous.clone();
        unproved.summaries[0].checksum ^= 1;
        assert!(delta(&unproved, &current, &existing).is_none());
        unproved = current.clone();
        unproved.segments.clear();
        assert!(delta(&previous, &unproved, &existing).is_none());

        let mut stale = existing.clone();
        stale
            .iter_mut()
            .find(|page| page.collection == "guarded_rows")
            .expect("reference")
            .checksum ^= 1;
        assert!(delta(&previous, &current, &stale).is_none());

        let document_id = EntityId::new(999_999);
        manager
            .insert(UnifiedEntity::table_row(
                document_id,
                "guarded_rows",
                99,
                vec![Value::Json(br#"{"field":"value"}"#.to_vec())],
            ))
            .expect("introduce document projection");
        current.segments = manager.checkpoint_revisions();
        assert!(delta(&previous, &current, &existing).is_none());
        let publication = db.prepare_native_artifact_publication(&existing);
        let reference = db.native_vector_artifact_records(&["guarded_rows".to_owned()]);
        let guarded: BTreeMap<_, _> = publication
            .records
            .into_iter()
            .filter(|(summary, _)| summary.collection == "guarded_rows")
            .map(|(summary, bytes)| (summary.artifact_kind, bytes))
            .collect();
        assert_eq!(
            guarded.len(),
            2,
            "new document projection must be published"
        );
        assert_eq!(
            guarded,
            reference
                .into_iter()
                .map(|(summary, bytes)| (summary.artifact_kind, bytes))
                .collect()
        );
        assert!(manager.delete(document_id).expect("remove test document"));
        current.segments = manager.checkpoint_revisions();
        assert!(delta(&previous, &current, &existing).is_some());

        let limit = crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX;
        for id in 0..=limit {
            manager
                .insert(UnifiedEntity::table_row(
                    EntityId::new(1_000_000 + id as u64),
                    "guarded_rows",
                    id as u64,
                    vec![Value::text("journal overflow")],
                ))
                .expect("fill journal");
        }
        current.segments = manager.checkpoint_revisions();
        assert!(delta(&previous, &current, &existing).is_none());
        let publication = db.prepare_native_artifact_publication(&existing);
        let reference = db.native_vector_artifact_records(&["guarded_rows".to_owned()]);
        assert_eq!(
            publication
                .records
                .iter()
                .find(|(summary, _)| summary.collection == "guarded_rows")
                .expect("full fallback")
                .1,
            reference[0].1
        );
    }

    #[test]
    fn tokenless_rows_force_full_rebuild_until_document_coverage_is_restored() {
        let directory = tempfile::tempdir().expect("temporary store");
        let options = RedDBOptions::persistent(directory.path().join("tokenless.rdb"))
            .with_storage_profile(crate::storage::StorageDeployPreset::Serverless.selection())
            .expect("paged profile");
        let runtime = crate::RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("CREATE TABLE tokenless_rows (payload TEXT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO tokenless_rows (payload) VALUES ('alpha')")
            .expect("seed");
        runtime.checkpoint().expect("baseline");
        let db = runtime.db();
        let manager = db.store.get_collection("tokenless_rows").expect("manager");
        let mut rows = Vec::new();
        manager.for_each_entity(|entity| {
            rows.push(entity.clone());
            true
        });
        assert_eq!(rows.len(), 1);
        let mut entity = rows.pop().expect("source row");
        for (text, incremental, complete) in [
            ("a", true, false),
            ("beta", false, true),
            ("gamma", true, true),
            ("", false, false),
            ("omega", false, true),
        ] {
            APPLIED_ROWS.with(|counts| counts.borrow_mut().clear());
            // Edit the same physical document. SQL edits may retain historical
            // MVCC rows, whose tokenless values must keep coverage incomplete.
            let EntityData::Row(row) = &mut entity.data else {
                panic!("table source must remain a row");
            };
            if let Some(fields) = &mut row.named {
                *fields.get_mut("payload").expect("named payload") = Value::text(text);
            } else {
                row.columns[0] = Value::text(text);
            }
            manager.update(entity.clone()).expect("physical edit");
            runtime.checkpoint().expect("publish edited rows");
            APPLIED_ROWS.with(|counts| {
                assert_eq!(
                    counts.borrow().contains_key("tokenless_rows"),
                    incremental,
                    "incremental admission for {text:?}"
                );
            });
            assert_eq!(
                db.native_artifact_versions.lock()["tokenless_rows"].fulltext_document_ids_complete,
                complete,
                "document coverage for {text:?}"
            );
            let header = db.store.physical_file_header().expect("header");
            let persisted = db
                .store
                .read_native_vector_artifact_blob(
                    header.vector_artifact_page,
                    "tokenless_rows",
                    Some("text.fulltext"),
                )
                .expect("published frame");
            let rebuilt = db.native_vector_artifact_records(&["tokenless_rows".to_owned()]);
            assert_eq!(
                persisted.map(|(_, bytes)| bytes),
                rebuilt.into_iter().next().map(|(_, bytes)| bytes)
            );
        }
        drop(db);
        drop(runtime);
        let reopened = crate::RedDBRuntime::with_options(options.clone()).expect("cold reopen");
        let rows = reopened
            .execute_query("SELECT payload FROM tokenless_rows")
            .expect("recovered row");
        assert_eq!(
            rows.result.records[0].get("payload"),
            Some(&Value::text("omega"))
        );
        // The recovered complete proof permits the first sparse edit. Its
        // tokenless document makes coverage incomplete, so the following edit
        // must still use the full builder while that historical row remains.
        for (text, incremental) in [("a", true), ("theta", false)] {
            APPLIED_ROWS.with(|counts| counts.borrow_mut().clear());
            reopened
                .execute_query(&format!("UPDATE tokenless_rows SET payload='{text}'"))
                .expect("SQL edit retaining MVCC history");
            reopened.checkpoint().expect("publish SQL history");
            let db = reopened.db();
            assert!(
                !db.native_artifact_versions.lock()["tokenless_rows"]
                    .fulltext_document_ids_complete,
                "tokenless historical documents still count after SQL edit {text:?}"
            );
            APPLIED_ROWS.with(|counts| {
                assert_eq!(
                    counts.borrow().contains_key("tokenless_rows"),
                    incremental,
                    "cold SQL history admission for {text:?}"
                );
            });
            let header = db.store.physical_file_header().expect("history header");
            let (_, persisted) = db
                .store
                .read_native_vector_artifact_blob(
                    header.vector_artifact_page,
                    "tokenless_rows",
                    Some("text.fulltext"),
                )
                .expect("read historical frame")
                .expect("historical frame");
            let reference = db.native_vector_artifact_records(&["tokenless_rows".to_owned()]);
            assert_eq!(persisted, reference[0].1);
        }
        drop(reopened);
        let recovered = crate::RedDBRuntime::with_options(options).expect("SQL history recovery");
        let rows = recovered
            .execute_query("SELECT payload FROM tokenless_rows")
            .expect("visible SQL payload after recovery");
        assert_eq!(rows.result.records.len(), 1);
        assert_eq!(
            rows.result.records[0].get("payload"),
            Some(&Value::text("theta"))
        );
    }
}
