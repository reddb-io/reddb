//! Driver routing cache updates from MOVED redirects.

use std::collections::BTreeMap;

use reddb_wire::MovedRedirect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedOwner {
    pub owner_addr: String,
    pub ownership_epoch: u64,
    pub catalog_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RangeKey {
    collection: String,
    range_id: u64,
    slot: Option<u64>,
}

#[derive(Debug, Default, Clone)]
pub struct RoutingCache {
    ranges: BTreeMap<RangeKey, CachedOwner>,
}

impl RoutingCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_moved(&mut self, moved: &MovedRedirect) -> bool {
        let key = RangeKey {
            collection: moved.collection.clone(),
            range_id: moved.range_id,
            slot: moved.slot,
        };
        let incoming = CachedOwner {
            owner_addr: moved.owner_addr.clone(),
            ownership_epoch: moved.ownership_epoch,
            catalog_version: moved.catalog_version,
        };

        match self.ranges.get(&key) {
            Some(existing)
                if existing.catalog_version > incoming.catalog_version
                    || existing.ownership_epoch > incoming.ownership_epoch =>
            {
                false
            }
            _ => {
                self.ranges.insert(key, incoming);
                true
            }
        }
    }

    pub fn owner_for(
        &self,
        collection: &str,
        range_id: u64,
        slot: Option<u64>,
    ) -> Option<&CachedOwner> {
        self.ranges.get(&RangeKey {
            collection: collection.to_string(),
            range_id,
            slot,
        })
    }

    pub fn retry_owner_after_moved(&mut self, moved: &MovedRedirect) -> &str {
        self.apply_moved(moved);
        &self
            .owner_for(&moved.collection, moved.range_id, moved.slot)
            .expect("MOVED update must populate retry owner")
            .owner_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(owner_addr: &str, epoch: u64, version: u64) -> MovedRedirect {
        MovedRedirect {
            slot: Some(12),
            collection: "orders".to_string(),
            range_id: 3,
            owner_addr: owner_addr.to_string(),
            ownership_epoch: epoch,
            catalog_version: version,
            reason: "transaction".to_string(),
        }
    }

    #[test]
    fn moved_redirect_updates_cache_and_retry_targets_current_owner() {
        let mut cache = RoutingCache::new();
        cache.apply_moved(&moved("node-a:5050", 1, 1));

        let retry_owner = cache.retry_owner_after_moved(&moved("node-b:5050", 2, 2));

        assert_eq!(retry_owner, "node-b:5050");
        assert_eq!(
            cache.owner_for("orders", 3, Some(12)),
            Some(&CachedOwner {
                owner_addr: "node-b:5050".to_string(),
                ownership_epoch: 2,
                catalog_version: 2,
            })
        );
    }

    #[test]
    fn stale_moved_payload_does_not_replace_newer_route() {
        let mut cache = RoutingCache::new();
        assert!(cache.apply_moved(&moved("node-b:5050", 3, 3)));

        assert!(!cache.apply_moved(&moved("node-a:5050", 2, 2)));

        assert_eq!(
            cache.owner_for("orders", 3, Some(12)).unwrap().owner_addr,
            "node-b:5050"
        );
    }
}
