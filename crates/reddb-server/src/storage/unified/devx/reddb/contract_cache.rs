#[cfg(test)]
use crate::physical::CollectionContract;
pub(super) use reddb_engine::catalog::cache::CollectionContractCache;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    fn fixture() -> CollectionContract {
        let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("KV PUT catalog.item = 1")
            .expect("seed");
        runtime
            .db()
            .collection_contract("catalog")
            .expect("contract")
    }

    fn race_rebuild(update: impl FnOnce(&CollectionContractCache), expected: Option<u32>) {
        let cache = CollectionContractCache::default();
        let old = fixture();
        let mut current = old.clone();
        current.version = 2;
        let loaded = Barrier::new(2);
        let release = Barrier::new(2);
        let result = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut first = true;
                cache.get_or_load(|| {
                    if first {
                        first = false;
                        let snapshot = old.clone();
                        loaded.wait();
                        release.wait();
                        vec![snapshot]
                    } else {
                        vec![current.clone()]
                    }
                })
            });
            loaded.wait();
            update(&cache);
            release.wait();
            reader.join().expect("loader")
        });
        assert_eq!(
            result.get("catalog").map(|contract| contract.version),
            expected
        );
        let cached = cache.get_or_load(|| panic!("successful load must be cached"));
        assert!(Arc::ptr_eq(&result, &cached));
    }

    #[test]
    fn contract_cache_rebuild_retries_after_invalidation() {
        race_rebuild(|cache| cache.invalidate(), Some(2));
    }

    #[test]
    fn contract_cache_rebuild_does_not_overwrite_published_contract() {
        race_rebuild(
            |cache| {
                let mut contract = fixture();
                contract.version = 2;
                cache.insert(contract);
            },
            Some(2),
        );
    }

    #[test]
    fn contract_cache_rebuild_does_not_resurrect_removed_contract() {
        race_rebuild(
            |cache| {
                cache.remove("catalog");
            },
            None,
        );
    }

    #[test]
    fn contract_cache_published_snapshots_remain_immutable() {
        let cache = CollectionContractCache::default();
        let mut contract = fixture();
        cache.insert(contract.clone());
        let original = cache.get_or_load(|| panic!("cached"));
        contract.version = 2;
        cache.replace([(contract.name.clone(), Arc::new(contract))].into());
        let updated = cache.get_or_load(|| panic!("cached"));
        assert_eq!(original["catalog"].version, 1);
        assert_eq!(updated["catalog"].version, 2);
    }
}
