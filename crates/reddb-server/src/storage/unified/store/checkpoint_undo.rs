//! Restore a checkpoint's first page images before loading its published trees.
use super::*;
use crate::storage::engine::{Page, PAGE_SIZE};
use reddb_file::page_ownership_journal::{encode, pending_epoch, PageOwnershipRecord};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn page_image(page_id: u32, bytes: &[u8]) -> io::Result<Page> {
    let image: [u8; PAGE_SIZE] = bytes
        .try_into()
        .map_err(|_| invalid("checkpoint before-image has the wrong page size"))?;
    let page = Page::from_bytes(image);
    if page.page_id() != page_id {
        return Err(invalid(
            "checkpoint before-image has the wrong page identity",
        ));
    }
    page.verify_checksum()
        .map_err(|error| invalid(&error.to_string()))?;
    page.page_type()
        .map_err(|error| invalid(&error.to_string()))?;
    Ok(page)
}

impl StoreCommitCoordinator {
    pub(crate) fn restore_checkpoint_page_images(&self, store: &UnifiedStore) -> io::Result<()> {
        let Some(pager) = &store.pager else {
            return Ok(());
        };
        let header = pager
            .header()
            .map_err(|error| invalid(&error.to_string()))?;
        if header.version < reddb_file::file_format::PAGE_CHECKPOINT_UNDO_FILE_VERSION {
            return Ok(());
        }

        // Validate the entire journal and every image before changing a page.
        // Keep page identities only; a second streaming pass restores images
        // one at a time instead of retaining their payloads in recovery RAM.
        let mut journal = Vec::new();
        let mut active_epoch = None;
        let mut images = roaring::RoaringBitmap::new();
        for item in WalReader::open(&self.wal_path)?.iter() {
            let (_, record) = match item {
                Ok(record) => record,
                Err(error)
                    if error.kind() == io::ErrorKind::UnexpectedEof && journal.is_empty() =>
                {
                    return Ok(());
                }
                Err(error) => {
                    return Err(invalid(&format!("incomplete checkpoint undo WAL: {error}")))
                }
            };
            match record {
                WalRecord::PageOwnership { record } => {
                    journal.extend_from_slice(&encode(record)?);
                    match record {
                        PageOwnershipRecord::Begin { epoch } => {
                            active_epoch = Some(epoch);
                            images.clear();
                        }
                        PageOwnershipRecord::Finish { .. } => {
                            active_epoch = None;
                            images.clear();
                        }
                        _ => {}
                    }
                }
                WalRecord::FullPageImage {
                    tx_id,
                    page_id,
                    ckpt_epoch,
                    data,
                } => {
                    if tx_id != 0
                        || active_epoch != Some(ckpt_epoch)
                        || page_id < reddb_engine::storage::pager_io::FIRST_ALLOCATABLE_PAGE
                        || page_id >= header.page_count
                        || !images.insert(page_id)
                    {
                        return Err(invalid("invalid checkpoint before-image ownership"));
                    }
                    page_image(page_id, &data)?;
                }
                _ => {}
            }
        }
        let Some(pending) = pending_epoch(&journal).map_err(|error| {
            invalid(&format!(
                "page ownership journal requires recovery: {error}"
            ))
        })?
        else {
            return Ok(());
        };
        if images.is_empty() {
            return Ok(());
        }
        if images
            .iter()
            .any(|page| pending.reserved_pages.contains(&page))
        {
            return Err(invalid("checkpoint before-image aliases a new reservation"));
        }
        let published_epoch = store
            .published_ownership_epoch()
            .map_err(|error| invalid(&error.to_string()))?;
        if published_epoch > pending.epoch {
            return Err(invalid("checkpoint undo precedes the published epoch"));
        }
        if published_epoch == pending.epoch {
            return Ok(());
        }

        for item in WalReader::open(&self.wal_path)?.iter() {
            let (_, record) = item?;
            if let WalRecord::FullPageImage {
                page_id,
                ckpt_epoch,
                data,
                ..
            } = record
            {
                if ckpt_epoch == pending.epoch {
                    pager
                        .write_page(page_id, page_image(page_id, &data)?)
                        .map_err(|error| invalid(&error.to_string()))?;
                    #[cfg(test)]
                    if std::env::var_os("REDDB_TEST_EXIT_DURING_PAGE_UNDO").is_some() {
                        pager.sync().map_err(|error| invalid(&error.to_string()))?;
                        std::process::exit(194);
                    }
                }
            }
        }
        // Restoration must survive another crash before allocator recovery can
        // release newly allocated children/overflow pages from the failed epoch.
        pager.sync().map_err(|error| invalid(&error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CHILD: &str = "REDDB_CHECKPOINT_PAGE_UNDO_CHILD";
    const TEST: &str = "storage::unified::store::commit::checkpoint_undo::tests::overflow_checkpoint_undo_survives_interrupted_recovery_and_publication";

    fn row(id: u64, generation: u64) -> UnifiedEntity {
        let mut random = id + generation * 137;
        let payload: String = (0..24 * 1024)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                char::from(b'!' + (random % 90) as u8)
            })
            .collect();
        UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![Value::text(payload)])
    }

    #[test]
    fn overflow_checkpoint_undo_survives_interrupted_recovery_and_publication() {
        let config = || UnifiedStoreConfig::default().with_page_cache_slots(4);
        if let Some(path) = std::env::var_os(CHILD) {
            let store = UnifiedStore::open_with_config(path, config()).expect("child open");
            if std::env::var_os("REDDB_TEST_EXIT_DURING_PAGE_UNDO").is_some() {
                panic!("recovery interruption was not reached");
            }
            for id in 1..=32 {
                store
                    .insert_auto("rows", row(id, 1))
                    .expect("seed overflow row");
            }
            store.persist().expect("baseline");
            let manager = store.get_collection("rows").expect("rows");
            let mut actions = Vec::new();
            for id in 1..=32 {
                manager.update(row(id, 2)).expect("changed row");
                let entity = manager.get(EntityId::new(id)).expect("post-image");
                actions.push(StoreWalAction::upsert_entity(
                    "rows",
                    &entity,
                    None,
                    store.format_version(),
                ));
            }
            let references = store
                .commit
                .as_ref()
                .expect("WAL")
                .append_checkpoint_actions(&actions)
                .expect("durable rows");
            store.remember_checkpoint_rows(actions, references);
            let fault = std::env::var("REDDB_PAGE_UNDO_FAULT").expect("fault");
            std::env::set_var(&fault, "1");
            store.persist().expect("injected checkpoint exit");
            panic!("checkpoint interruption was not reached");
        }
        for (fault, exit_code) in [
            ("REDDB_TEST_EXIT_AFTER_SPARSE_BATCH", 193),
            ("REDDB_TEST_EXIT_AFTER_UNDO_CATALOG_PUBLICATION", 195),
        ] {
            let directory = tempfile::tempdir().expect("temporary database");
            let path = directory.path().join("undo.rdb");
            let run = |recovery: bool| {
                let mut command =
                    std::process::Command::new(std::env::current_exe().expect("test binary"));
                command
                    .args(["--exact", TEST, "--nocapture"])
                    .env(CHILD, &path)
                    .env("REDDB_PAGE_UNDO_FAULT", fault);
                if recovery {
                    command.env("REDDB_TEST_EXIT_DURING_PAGE_UNDO", "1");
                }
                command.output().expect("child process")
            };
            let result = run(false);
            assert_eq!(
                result.status.code(),
                Some(exit_code),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let images = WalReader::open(reddb_file::layout::unified_wal_path(&path))
                .expect("WAL")
                .iter()
                .filter(|item| matches!(item, Ok((_, WalRecord::FullPageImage { .. }))))
                .count();
            assert!(images > 0, "the fixture must overwrite published pages");
            if exit_code == 193 {
                let wal_path = reddb_file::layout::unified_wal_path(&path);
                let baseline_wal = std::fs::read(&wal_path).expect("baseline WAL");
                let baseline_pages = std::fs::read(&path).expect("baseline pages");
                let (image_offset, original_image) = WalReader::open(&wal_path)
                    .expect("WAL")
                    .iter()
                    .filter_map(|item| match item.expect("valid record") {
                        (offset, image @ WalRecord::FullPageImage { .. }) => Some((offset, image)),
                        _ => None,
                    })
                    .last()
                    .expect("before-image");
                for corruption in 0..6 {
                    let mut image = original_image.clone();
                    if let WalRecord::FullPageImage {
                        tx_id,
                        page_id,
                        ckpt_epoch,
                        data,
                    } = &mut image
                    {
                        match corruption {
                            0 => *tx_id = 9,
                            1 => *page_id = 1,
                            2 => *ckpt_epoch += 1,
                            3 => {
                                data.pop();
                            }
                            4 => data[64] ^= 1,
                            5 => {} // Duplicate a valid first image.
                            _ => unreachable!(),
                        }
                    }
                    let mut invalid_wal = baseline_wal.clone();
                    if corruption == 5 {
                        invalid_wal.extend(image.encode());
                    } else {
                        let offset = usize::try_from(image_offset).expect("fixture offset");
                        invalid_wal.splice(
                            offset..offset + original_image.encode().len(),
                            image.encode(),
                        );
                    }
                    std::fs::write(&wal_path, invalid_wal).expect("inject invalid image");
                    assert!(
                        UnifiedStore::open_with_config(&path, config()).is_err(),
                        "corruption {corruption}"
                    );
                    assert_eq!(
                        std::fs::read(&path).expect("unchanged pages"),
                        baseline_pages,
                        "validate all images before restoring any page"
                    );
                }
                let mut truncated = baseline_wal.clone();
                let frame = original_image.encode();
                truncated.extend_from_slice(&frame[..frame.len() / 2]);
                std::fs::write(&wal_path, truncated).expect("incomplete undo WAL");
                assert!(UnifiedStore::open_with_config(&path, config()).is_err());
                assert_eq!(
                    std::fs::read(&path).expect("unchanged pages"),
                    baseline_pages
                );
                std::fs::write(&wal_path, baseline_wal).expect("restore valid WAL");
                for _ in 0..2 {
                    let result = run(true);
                    assert_eq!(
                        result.status.code(),
                        Some(194),
                        "{}",
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
            }
            for _ in 0..3 {
                let store =
                    UnifiedStore::open_with_config(&path, config()).expect("recover checkpoint");
                assert_eq!(store.get_collection("rows").expect("rows").count(), 32);
                for id in 1..=32 {
                    assert_eq!(
                        store
                            .get("rows", EntityId::new(id))
                            .expect("recovered row")
                            .data
                            .as_row()
                            .expect("row")
                            .columns,
                        row(id, 2).data.as_row().expect("expected row").columns
                    );
                }
                store.persist().expect("retirement after recovery");
            }
        }
    }
}
