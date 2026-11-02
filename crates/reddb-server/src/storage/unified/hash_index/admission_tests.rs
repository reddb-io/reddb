use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Weak;

struct TestPagedProvider;
impl PagedHashIndex for TestPagedProvider {
    fn lookup(&self, _: &[u8]) -> Result<HashCandidates, HashIndexError> {
        Ok(vec![EntityId::new(99)].into())
    }
    fn memory_bytes(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy)]
enum Replacement {
    Resident,
    Paged,
    Missing,
    Empty,
}

struct ReplacingAdmission {
    manager: Weak<HashIndexManager>,
    replacement: Replacement,
    samples: AtomicUsize,
    reservations: AtomicUsize,
}

impl HashCandidateAdmission for ReplacingAdmission {
    fn sample(&self) -> Result<(), HashIndexError> {
        let manager = self.manager.upgrade().unwrap();
        let guard = manager
            .indices
            .try_write()
            .expect("sampling must run outside the index lock");
        drop(guard);
        assert_eq!(self.samples.fetch_add(1, Ordering::SeqCst), 0);
        let prepared = HashIndexManager::new();
        match self.replacement {
            Replacement::Paged => {
                prepared.install_paged("table", "index", Arc::new(TestPagedProvider))
            }
            Replacement::Missing => (),
            Replacement::Resident | Replacement::Empty => {
                prepared.create_index(&config()).unwrap();
                if matches!(self.replacement, Replacement::Resident) {
                    prepared
                        .insert("table", "index", b"key".to_vec(), EntityId::new(2))
                        .unwrap();
                    prepared
                        .insert("table", "index", b"key".to_vec(), EntityId::new(3))
                        .unwrap();
                }
            }
        }
        manager.replace_collection_from("table", prepared);
        Ok(())
    }

    fn reserve(&self, bytes: usize) -> Result<Box<dyn Send + Sync>, HashIndexError> {
        let manager = self.manager.upgrade().unwrap();
        assert!(
            manager.indices.try_write().is_none(),
            "posting lengths must stay frozen through admission"
        );
        assert_eq!(bytes, 2 * std::mem::size_of::<EntityId>());
        self.reservations.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(()))
    }
}

fn config() -> HashIndexConfig {
    HashIndexConfig {
        collection: "table".into(),
        name: "index".into(),
        columns: vec!["key".into()],
        unique: false,
    }
}

#[test]
fn hash_candidate_admission_samples_unlocked_and_rechecks_replaced_backings() {
    for union in [false, true] {
        for replacement in [
            Replacement::Resident,
            Replacement::Paged,
            Replacement::Missing,
            Replacement::Empty,
        ] {
            let manager = Arc::new(HashIndexManager::new());
            manager.create_index(&config()).unwrap();
            manager
                .insert("table", "index", b"key".to_vec(), EntityId::new(1))
                .unwrap();
            let admission = Arc::new(ReplacingAdmission {
                manager: Arc::downgrade(&manager),
                replacement,
                samples: AtomicUsize::new(0),
                reservations: AtomicUsize::new(0),
            });
            manager.install_candidate_admission(admission.clone());
            let result = if union {
                manager.lookup_union_owned("table", "index", std::iter::once(b"key"))
            } else {
                manager.lookup_owned("table", "index", b"key").map(Some)
            };
            assert_eq!(admission.samples.load(Ordering::SeqCst), 1);
            match replacement {
                Replacement::Resident => {
                    assert_eq!(
                        result.unwrap().unwrap(),
                        vec![EntityId::new(2), EntityId::new(3)]
                    );
                    assert_eq!(admission.reservations.load(Ordering::SeqCst), 1);
                }
                Replacement::Paged if union => assert!(result.unwrap().is_none()),
                Replacement::Paged => assert_eq!(result.unwrap().unwrap(), vec![EntityId::new(99)]),
                Replacement::Empty => assert!(result.unwrap().unwrap().is_empty()),
                Replacement::Missing => {
                    assert!(matches!(result, Err(HashIndexError::MissingIndex { .. })))
                }
            }
            if !matches!(replacement, Replacement::Resident) {
                assert_eq!(admission.reservations.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[test]
fn hash_candidate_unmanaged_union_preserves_each_key_and_legacy_ownership() {
    let manager = HashIndexManager::new();
    manager.create_index(&config()).unwrap();
    manager
        .insert("table", "index", b"first".to_vec(), EntityId::new(1))
        .unwrap();
    manager
        .insert("table", "index", b"second".to_vec(), EntityId::new(2))
        .unwrap();
    let result = manager
        .lookup_union_owned(
            "table",
            "index",
            [b"second".as_slice(), b"absent", b"first"].into_iter(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        result.into_resident().unwrap(),
        vec![EntityId::new(2), EntityId::new(1)]
    );
}
