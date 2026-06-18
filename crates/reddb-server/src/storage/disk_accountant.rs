use std::collections::HashSet;

use crate::storage::engine::{PageType, HEADER_SIZE, PAGE_SIZE};
use crate::storage::unified::UnifiedStore;

const INTERIOR_DATA_OFFSET: usize = HEADER_SIZE;

pub(crate) fn bytes_on_disk_for(store: &UnifiedStore, collection: &str) -> Option<u64> {
    estimate_bytes_on_disk(store, collection)
}

fn estimate_bytes_on_disk(store: &UnifiedStore, collection: &str) -> Option<u64> {
    store.db_path()?;
    store.pager()?;
    let Some(root_page) = store.collection_root_page(collection) else {
        return Some(0);
    };
    let Some(pages) = reachable_btree_pages(store, root_page) else {
        return None;
    };
    Some(pages.saturating_mul(PAGE_SIZE as u64))
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
