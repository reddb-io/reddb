use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::physical::CollectionContract;

pub(super) type ContractMap = HashMap<String, Arc<CollectionContract>>;

#[derive(Default)]
struct CacheState {
    generation: u64,
    contracts: Option<Arc<ContractMap>>,
}

impl CacheState {
    fn advance_generation(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("collection contract cache generation overflow");
    }
}

/// Immutable reader snapshots; metadata I/O never holds the cache lock.
#[derive(Default)]
pub(super) struct CollectionContractCache {
    state: RwLock<CacheState>,
}

impl CollectionContractCache {
    pub(super) fn get_or_load(
        &self,
        mut load: impl FnMut() -> Vec<CollectionContract>,
    ) -> Arc<ContractMap> {
        loop {
            let generation = {
                let state = self.state.read();
                if let Some(contracts) = &state.contracts {
                    return Arc::clone(contracts);
                }
                state.generation
            };
            let contracts = Arc::new(
                load()
                    .into_iter()
                    .map(|contract| (contract.name.clone(), Arc::new(contract)))
                    .collect(),
            );
            let mut state = self.state.write();
            if let Some(published) = &state.contracts {
                return Arc::clone(published);
            }
            // Metadata may have changed after load() took its snapshot. Never
            // publish that snapshot across an invalidation or definition edit.
            if state.generation != generation {
                continue;
            }
            state.contracts = Some(Arc::clone(&contracts));
            return contracts;
        }
    }

    pub(super) fn invalidate(&self) {
        let mut state = self.state.write();
        state.advance_generation();
        state.contracts = None;
    }

    pub(super) fn replace(&self, contracts: ContractMap) {
        let mut state = self.state.write();
        state.advance_generation();
        state.contracts = Some(Arc::new(contracts));
    }

    pub(super) fn insert(&self, contract: CollectionContract) {
        let mut state = self.state.write();
        state.advance_generation();
        let contracts = state.contracts.get_or_insert_with(Arc::default);
        Arc::make_mut(contracts).insert(contract.name.clone(), Arc::new(contract));
    }

    pub(super) fn remove(&self, name: &str) -> Option<CollectionContract> {
        let mut state = self.state.write();
        state.advance_generation();
        let contracts = state.contracts.get_or_insert_with(Arc::default);
        Arc::make_mut(contracts)
            .remove(name)
            .map(|contract| (*contract).clone())
    }
}

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
