//! Immutable catalog reader snapshots; host metadata I/O stays outside cache locks.
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use super::CollectionContract;

pub type ContractMap = HashMap<String, Arc<CollectionContract>>;

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
pub struct CollectionContractCache {
    state: RwLock<CacheState>,
}

impl CollectionContractCache {
    pub fn get_or_load(
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

    pub fn invalidate(&self) {
        let mut state = self.state.write();
        state.advance_generation();
        state.contracts = None;
    }

    pub fn replace(&self, contracts: ContractMap) {
        let mut state = self.state.write();
        state.advance_generation();
        state.contracts = Some(Arc::new(contracts));
    }

    pub fn insert(&self, contract: CollectionContract) {
        let mut state = self.state.write();
        state.advance_generation();
        let contracts = state.contracts.get_or_insert_with(Arc::default);
        Arc::make_mut(contracts).insert(contract.name.clone(), Arc::new(contract));
    }

    pub fn remove(&self, name: &str) -> Option<CollectionContract> {
        let mut state = self.state.write();
        state.advance_generation();
        let contracts = state.contracts.get_or_insert_with(Arc::default);
        Arc::make_mut(contracts)
            .remove(name)
            .map(|contract| (*contract).clone())
    }
}
