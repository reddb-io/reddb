use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::storage::engine::{PageType, HEADER_SIZE, PAGE_SIZE};
use crate::storage::unified::UnifiedStore;

const CACHE_TTL: Duration = Duration::from_secs(30);
const INTERIOR_DATA_OFFSET: usize = HEADER_SIZE;

type Cache = HashMap<String, (u64, Instant)>;

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

pub(crate) fn bytes_on_disk_for(store: &UnifiedStore, collection: &str) -> u64 {
    let key = cache_key(store, collection);
    let now = Instant::now();

    if let Some(bytes) = cached_bytes(&key, now) {
        return bytes;
    }

    let bytes = estimate_bytes_on_disk(store, collection);
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(key, (bytes, now));
    bytes
}

fn cached_bytes(key: &str, now: Instant) -> Option<u64> {
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match cache.get(key).copied() {
        Some((bytes, measured_at)) if now.duration_since(measured_at) < CACHE_TTL => Some(bytes),
        Some(_) => {
            cache.remove(key);
            None
        }
        None => None,
    }
}

fn cache_key(store: &UnifiedStore, collection: &str) -> String {
    let db = store
        .db_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<memory>".to_string());
    format!("{db}\0{collection}")
}

fn estimate_bytes_on_disk(store: &UnifiedStore, collection: &str) -> u64 {
    let Some(root_page) = store.collection_root_page(collection) else {
        return 0;
    };
    let Some(pages) = reachable_btree_pages(store, root_page) else {
        return 0;
    };
    pages.saturating_mul(PAGE_SIZE as u64)
}

fn reachable_btree_pages(store: &UnifiedStore, root_page: u32) -> Option<u64> {
    let pager = store.pager()?;
    let mut stack = vec![root_page];
    let mut visited = HashSet::new();

    while let Some(page_id) = stack.pop() {
        if page_id == 0 || !visited.insert(page_id) {
            continue;
        }

        let page = pager.read_page(page_id).ok()?;
        match page.page_type().ok()? {
            PageType::BTreeLeaf => {}
            PageType::BTreeInterior => stack.extend(interior_children(&page)?),
            _ => return None,
        }
    }

    Some(visited.len() as u64)
}

fn interior_children(page: &crate::storage::engine::Page) -> Option<Vec<u32>> {
    let data = page.as_bytes();
    let cell_count = page.cell_count() as usize;
    let mut children = Vec::with_capacity(cell_count + 1);

    for index in 0..cell_count {
        let slot_pos = INTERIOR_DATA_OFFSET.checked_add(index.checked_mul(2)?)?;
        if slot_pos + 2 > PAGE_SIZE {
            return None;
        }
        let offset = u16::from_le_bytes([data[slot_pos], data[slot_pos + 1]]) as usize;
        if offset + 2 > PAGE_SIZE {
            return None;
        }
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let child_pos = offset.checked_add(2)?.checked_add(key_len)?;
        if child_pos + 4 > PAGE_SIZE {
            return None;
        }
        children.push(u32::from_le_bytes([
            data[child_pos],
            data[child_pos + 1],
            data[child_pos + 2],
            data[child_pos + 3],
        ]));
    }

    let right_child = page.right_child();
    if right_child != 0 {
        children.push(right_child);
    }

    Some(children)
}
