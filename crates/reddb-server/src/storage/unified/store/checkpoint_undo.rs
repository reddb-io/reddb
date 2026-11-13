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
        Self::restore_checkpoint_page_images_from_path(store, &self.wal_path)
    }

    pub(crate) fn restore_checkpoint_page_images_from_path(
        store: &UnifiedStore,
        wal_path: &Path,
    ) -> io::Result<()> {
        if !wal_path.exists() {
            return Ok(());
        }
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
        for item in WalReader::open(wal_path)?.iter() {
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

        #[cfg(test)]
        if let Ok(fault) = std::env::var("REDDB_TEST_PAGE_UNDO_IO") {
            use crate::storage::engine::pager;
            match fault.as_str() {
                "empty" => pager::test_fail_next_pager_write(None, 0),
                "partial" => pager::test_fail_next_pager_write(None, 17),
                "sync" => pager::test_fail_pager_sync_after(0),
                _ => panic!("unknown undo recovery I/O fault {fault}"),
            }
        }

        for item in WalReader::open(wal_path)?.iter() {
            let (_, record) = item?;
            if let WalRecord::FullPageImage {
                page_id,
                ckpt_epoch,
                data,
                ..
            } = record
            {
                if ckpt_epoch == pending.epoch {
                    let image = page_image(page_id, &data)?;
                    if pager.is_read_only() {
                        pager.restore_read_only_page(page_id, image)
                    } else {
                        pager.write_page(page_id, image)
                    }
                    .map_err(|error| invalid(&error.to_string()))?;
                    #[cfg(test)]
                    if !pager.is_read_only()
                        && std::env::var_os("REDDB_TEST_EXIT_DURING_PAGE_UNDO").is_some()
                    {
                        pager.sync().map_err(|error| invalid(&error.to_string()))?;
                        std::process::exit(194);
                    }
                }
            }
        }
        // Restoration must survive another crash before allocator recovery can
        // release newly allocated children/overflow pages from the failed epoch.
        if pager.is_read_only() {
            return Ok(());
        }
        pager.sync().map_err(|error| invalid(&error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CHILD: &str = "REDDB_CHECKPOINT_PAGE_UNDO_CHILD";
    const TEST: &str = "storage::unified::store::commit::checkpoint_undo::tests::overflow_checkpoint_undo_survives_interrupted_recovery_and_publication";

    fn run_child(command: &mut std::process::Command) -> std::process::Output {
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("recovery child");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while child.try_wait().expect("child status").is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().expect("stop stuck recovery");
                let output = child.wait_with_output().expect("stuck recovery output");
                panic!(
                    "recovery child exceeded 60s\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.wait_with_output().expect("recovery child output")
    }

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
            eprintln!("checkpoint undo: opening store");
            if std::env::var_os("REDDB_TEST_PAGE_UNDO_IO").is_some()
                || std::env::var_os("REDDB_TEST_PAGE_REPLAY_IO").is_some()
            {
                let error = match UnifiedStore::open_with_config(path, config()) {
                    Ok(_) => panic!("undo recovery must propagate its I/O failure"),
                    Err(error) => error,
                };
                assert!(error.to_string().contains("injected pager"), "{error}");
                std::process::exit(196);
            }
            let store = UnifiedStore::open_with_config(path, config()).expect("child open");
            if std::env::var_os("REDDB_TEST_EXIT_DURING_PAGE_UNDO").is_some() {
                panic!("recovery interruption was not reached");
            }
            eprintln!("checkpoint undo: seeding overflow rows");
            for id in 1..=32 {
                store
                    .insert_auto("rows", row(id, 1))
                    .expect("seed overflow row");
            }
            eprintln!("checkpoint undo: persisting baseline");
            store.persist().expect("baseline");
            eprintln!("checkpoint undo: updating overflow rows");
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
            eprintln!("checkpoint undo: appending durable redo");
            let references = store
                .commit
                .as_ref()
                .expect("WAL")
                .append_checkpoint_actions(&actions)
                .expect("durable rows");
            store.remember_checkpoint_rows(actions, references);
            let fault = std::env::var("REDDB_PAGE_UNDO_FAULT").expect("fault");
            eprintln!("checkpoint undo: interrupting publication at {fault}");
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
                run_child(&mut command)
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
            let original_wal =
                std::fs::read(reddb_file::layout::unified_wal_path(&path)).expect("preserved WAL");
            let original_pages = std::fs::read(&path).expect("preserved pages");
            for _ in 0..2 {
                let reader = UnifiedStore::open_read_only_with_config(
                    &path,
                    UnifiedStoreConfig::default().with_page_cache_slots(512),
                )
                .expect("read-only checkpoint undo and cold boot");
                assert_eq!(reader.get_collection("rows").expect("rows").count(), 32);
                for id in 1..=32 {
                    assert_eq!(
                        reader
                            .get("rows", EntityId::new(id))
                            .expect("row")
                            .data
                            .as_row()
                            .expect("row")
                            .columns,
                        row(id, 2).data.as_row().expect("expected row").columns
                    );
                }
                drop(reader);
                assert_eq!(
                    std::fs::read(&path).expect("unchanged pages"),
                    original_pages
                );
                assert_eq!(
                    std::fs::read(reddb_file::layout::unified_wal_path(&path))
                        .expect("unchanged WAL"),
                    original_wal
                );
            }
            let isolated = directory.path().join("isolated-recovery");
            crate::storage::compact::recover_paged_wal(&path, &isolated)
                .expect("recover interrupted checkpoint on a copy");
            assert_eq!(
                std::fs::read(&path).expect("unchanged pages"),
                original_pages
            );
            assert_eq!(
                std::fs::read(reddb_file::layout::unified_wal_path(&path)).expect("unchanged WAL"),
                original_wal
            );
            let recovered = UnifiedStore::open_with_config(isolated.join("undo.rdb"), config())
                .expect("open isolated recovery");
            assert_eq!(recovered.get_collection("rows").expect("rows").count(), 32);
            for id in 1..=32 {
                assert_eq!(
                    recovered
                        .get("rows", EntityId::new(id))
                        .expect("row")
                        .data
                        .as_row()
                        .expect("row")
                        .columns,
                    row(id, 2).data.as_row().expect("expected row").columns
                );
            }
            drop(recovered);
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
                    let invalid_wal_before = std::fs::read(&wal_path).expect("invalid WAL");
                    assert!(
                        UnifiedStore::open_read_only_with_config(&path, config()).is_err(),
                        "read-only corruption {corruption}"
                    );
                    assert_eq!(
                        std::fs::read(&wal_path).expect("unchanged invalid WAL"),
                        invalid_wal_before
                    );
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
                for fault in ["empty", "partial", "sync"] {
                    let wal = std::fs::read(&wal_path).expect("undo WAL before I/O failure");
                    let output = run_child(
                        std::process::Command::new(std::env::current_exe().expect("test binary"))
                            .args(["--exact", TEST, "--nocapture"])
                            .env(CHILD, &path)
                            .env("REDDB_TEST_PAGE_UNDO_IO", fault),
                    );
                    assert_eq!(
                        output.status.code(),
                        Some(196),
                        "{fault}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert_eq!(
                        std::fs::read(&wal_path).expect("retained undo WAL"),
                        wal,
                        "failed undo must not consume its before-images"
                    );
                }
                let replay_pages = std::fs::read(&path).expect("replay fixture pages");
                let replay_wal = std::fs::read(&wal_path).expect("replay fixture WAL");
                for fault in ["empty", "partial", "sync"] {
                    let replay_directory = tempfile::tempdir().expect("isolated replay fixture");
                    let replay_path = replay_directory.path().join("replay.rdb");
                    let replay_wal_path = reddb_file::layout::unified_wal_path(&replay_path);
                    std::fs::create_dir_all(replay_wal_path.parent().expect("WAL parent"))
                        .expect("replay support directory");
                    std::fs::write(&replay_path, &replay_pages).expect("copy replay pages");
                    std::fs::write(&replay_wal_path, &replay_wal).expect("copy replay WAL");
                    for _ in 0..2 {
                        let output = run_child(
                            std::process::Command::new(
                                std::env::current_exe().expect("test binary"),
                            )
                            .args(["--exact", TEST, "--nocapture"])
                            .env(CHILD, &replay_path)
                            .env("REDDB_TEST_PAGE_REPLAY_IO", fault),
                        );
                        assert_eq!(
                            output.status.code(),
                            Some(196),
                            "replay/{fault}: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                        assert!(
                            std::fs::read(&replay_wal_path)
                                .expect("retained replay WAL")
                                .starts_with(&replay_wal),
                            "replay may append ownership evidence but must retain committed WAL"
                        );
                    }
                    for _ in 0..2 {
                        let recovered = UnifiedStore::open_with_config(&replay_path, config())
                            .expect("recover interrupted replay publication");
                        assert_eq!(recovered.get_collection("rows").expect("rows").count(), 32);
                        for id in 1..=32 {
                            assert_eq!(
                                recovered
                                    .get("rows", EntityId::new(id))
                                    .expect("replayed row")
                                    .data
                                    .as_row()
                                    .expect("row")
                                    .columns,
                                row(id, 2).data.as_row().expect("expected row").columns
                            );
                        }
                        recovered.persist().expect("publish recovered replay");
                    }
                }
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
