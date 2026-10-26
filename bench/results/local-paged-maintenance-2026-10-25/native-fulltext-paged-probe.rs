//! Isolated algorithm experiment, not a persisted RedDB index implementation.
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use reddb_file::{NativeFulltextFrame, NativeFulltextPosting, NativeFulltextTerm};

#[path = "../../../crates/reddb-server/src/storage/unified/devx/reddb/native_fulltext_delta.rs"]
mod existing_delta;

#[derive(Clone, Copy, Default)]
struct Summary {
    len: u64,
    crc: u32,
}
impl Summary {
    fn bytes(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len() as u64,
            crc: crc32fast::hash(bytes),
        }
    }
    fn then(self, other: Self) -> Self {
        let mut combined = crc32fast::Hasher::new_with_initial_len(self.crc, self.len);
        combined.combine(&crc32fast::Hasher::new_with_initial_len(
            other.crc, other.len,
        ));
        Self {
            len: self.len.checked_add(other.len).expect("aggregate length"),
            crc: combined.finalize(),
        }
    }
}

trait Record: Clone {
    fn summary(&self) -> Summary;
}
type Link<K, V> = Option<Arc<Node<K, V>>>;
struct Node<K, V> {
    id: u64,
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u32,
    entries: u32,
    summary: Summary,
    generation: u64,
}
#[derive(Default)]
struct Work {
    visits: usize,
    allocated: usize,
}
fn height<K, V>(tree: &Link<K, V>) -> u32 {
    tree.as_ref().map_or(0, |n| n.height)
}
fn entries<K, V>(tree: &Link<K, V>) -> u32 {
    tree.as_ref().map_or(0, |n| n.entries)
}
fn summary<K, V>(tree: &Link<K, V>) -> Summary {
    tree.as_ref().map_or(Summary::default(), |n| n.summary)
}
fn node<K: Clone, V: Record>(
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    generation: u64,
    work: &mut Work,
) -> Arc<Node<K, V>> {
    work.allocated += 1;
    let aggregate = summary(&left).then(value.summary()).then(summary(&right));
    Arc::new(Node {
        id: NEXT_NODE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        key,
        value,
        height: 1 + height(&left).max(height(&right)),
        entries: entries(&left)
            .checked_add(1)
            .and_then(|v| v.checked_add(entries(&right)))
            .expect("entry count"),
        summary: aggregate,
        left,
        right,
        generation,
    })
}
fn balanced<K: Clone, V: Record>(
    key: K,
    value: V,
    mut left: Link<K, V>,
    mut right: Link<K, V>,
    generation: u64,
    work: &mut Work,
) -> Arc<Node<K, V>> {
    if height(&left) > height(&right) + 1 {
        let l = left.as_ref().expect("left-heavy node");
        if height(&l.left) < height(&l.right) {
            let lr = l.right.as_ref().expect("inner rotation");
            let new_left = node(
                l.key.clone(),
                l.value.clone(),
                l.left.clone(),
                lr.left.clone(),
                generation,
                work,
            );
            left = Some(node(
                lr.key.clone(),
                lr.value.clone(),
                Some(new_left),
                lr.right.clone(),
                generation,
                work,
            ));
        }
        let l = left.as_ref().expect("rotated left");
        let new_right = node(key, value, l.right.clone(), right, generation, work);
        return node(
            l.key.clone(),
            l.value.clone(),
            l.left.clone(),
            Some(new_right),
            generation,
            work,
        );
    }
    if height(&right) > height(&left) + 1 {
        let r = right.as_ref().expect("right-heavy node");
        if height(&r.right) < height(&r.left) {
            let rl = r.left.as_ref().expect("inner rotation");
            let new_right = node(
                r.key.clone(),
                r.value.clone(),
                rl.right.clone(),
                r.right.clone(),
                generation,
                work,
            );
            right = Some(node(
                rl.key.clone(),
                rl.value.clone(),
                rl.left.clone(),
                Some(new_right),
                generation,
                work,
            ));
        }
        let r = right.as_ref().expect("rotated right");
        let new_left = node(key, value, left, r.left.clone(), generation, work);
        return node(
            r.key.clone(),
            r.value.clone(),
            Some(new_left),
            r.right.clone(),
            generation,
            work,
        );
    }
    node(key, value, left, right, generation, work)
}
fn find<K: Ord, V>(tree: &Link<K, V>, key: &K, work: &mut Work) -> Option<Arc<Node<K, V>>> {
    let mut current = tree.clone();
    while let Some(n) = current {
        work.visits += 1;
        match key.cmp(&n.key) {
            std::cmp::Ordering::Less => current = n.left.clone(),
            std::cmp::Ordering::Greater => current = n.right.clone(),
            std::cmp::Ordering::Equal => return Some(n),
        }
    }
    None
}
fn put<K: Ord + Clone, V: Record>(
    tree: &Link<K, V>,
    key: K,
    value: Option<V>,
    generation: u64,
    work: &mut Work,
) -> Link<K, V> {
    let Some(n) = tree else {
        return value.map(|v| node(key, v, None, None, generation, work));
    };
    work.visits += 1;
    let (left, right) = match key.cmp(&n.key) {
        std::cmp::Ordering::Less => (put(&n.left, key, value, generation, work), n.right.clone()),
        std::cmp::Ordering::Greater => {
            (n.left.clone(), put(&n.right, key, value, generation, work))
        }
        std::cmp::Ordering::Equal => {
            if let Some(value) = value {
                return Some(node(
                    n.key.clone(),
                    value,
                    n.left.clone(),
                    n.right.clone(),
                    generation,
                    work,
                ));
            }
            match (&n.left, &n.right) {
                (None, _) => return n.right.clone(),
                (_, None) => return n.left.clone(),
                (Some(_), Some(r)) => {
                    let mut successor = Arc::clone(r);
                    while let Some(left) = &successor.left {
                        work.visits += 1;
                        successor = Arc::clone(left);
                    }
                    let right = put(&n.right, successor.key.clone(), None, generation, work);
                    return Some(balanced(
                        successor.key.clone(),
                        successor.value.clone(),
                        n.left.clone(),
                        right,
                        generation,
                        work,
                    ));
                }
            }
        }
    };
    Some(balanced(
        n.key.clone(),
        n.value.clone(),
        left,
        right,
        generation,
        work,
    ))
}
fn visit<K, V>(tree: &Link<K, V>, f: &mut impl FnMut(&Node<K, V>)) {
    if let Some(n) = tree {
        visit(&n.left, f);
        f(n);
        visit(&n.right, f);
    }
}
fn check<K: Ord, V: Record>(tree: &Link<K, V>) -> (u32, u32, Summary) {
    let Some(n) = tree else {
        return (0, 0, Summary::default());
    };
    let (lh, ln, ls) = check(&n.left);
    let (rh, rn, rs) = check(&n.right);
    assert!(lh.abs_diff(rh) <= 1);
    assert_eq!(n.height, 1 + lh.max(rh));
    assert_eq!(n.entries, ln + rn + 1);
    let combined = ls.then(n.value.summary()).then(rs);
    assert_eq!(n.summary.len, combined.len);
    assert_eq!(n.summary.crc, combined.crc);
    (n.height, n.entries, n.summary)
}

#[derive(Clone)]
struct Posting {
    id: u64,
    frequency: u32,
}
impl Posting {
    fn bytes(&self) -> [u8; 12] {
        let mut b = [0; 12];
        b[..8].copy_from_slice(&self.id.to_le_bytes());
        b[8..].copy_from_slice(&self.frequency.to_le_bytes());
        b
    }
}
impl Record for Posting {
    fn summary(&self) -> Summary {
        Summary::bytes(&self.bytes())
    }
}
#[derive(Clone)]
struct Term {
    name: Arc<str>,
    postings: Link<u64, Posting>,
    digest: Summary,
}
impl Term {
    fn new(name: Arc<str>, postings: Link<u64, Posting>) -> Self {
        let mut header = Vec::new();
        header.extend_from_slice(&(name.len() as u32).to_le_bytes());
        header.extend_from_slice(name.as_bytes());
        header.extend_from_slice(&entries(&postings).to_le_bytes());
        let digest = Summary::bytes(&header).then(summary(&postings));
        Self {
            name,
            postings,
            digest,
        }
    }
}
impl Record for Term {
    fn summary(&self) -> Summary {
        self.digest
    }
}

#[derive(Clone)]
struct Snapshot {
    collection: Arc<str>,
    terms: Link<Arc<str>, Term>,
    doc_count: u32,
}
impl Snapshot {
    fn header(&self) -> Vec<u8> {
        let mut header = b"RDFT".to_vec();
        header.extend_from_slice(&(self.collection.len() as u32).to_le_bytes());
        header.extend_from_slice(self.collection.as_bytes());
        header.extend_from_slice(&self.doc_count.to_le_bytes());
        header.extend_from_slice(&entries(&self.terms).to_le_bytes());
        header
    }
    fn digest(&self) -> Summary {
        Summary::bytes(&self.header()).then(summary(&self.terms))
    }
    fn bytes(&self) -> Vec<u8> {
        let mut out = self.header();
        visit(&self.terms, &mut |n| {
            out.extend_from_slice(&(n.value.name.len() as u32).to_le_bytes());
            out.extend_from_slice(n.value.name.as_bytes());
            out.extend_from_slice(&entries(&n.value.postings).to_le_bytes());
            visit(&n.value.postings, &mut |p| {
                out.extend_from_slice(&p.value.bytes())
            });
        });
        out
    }
    fn pointers(&self) -> HashSet<usize> {
        let mut seen = HashSet::new();
        fn collect<K, V>(tree: &Link<K, V>, seen: &mut HashSet<usize>) {
            if let Some(n) = tree {
                if seen.insert(Arc::as_ptr(n) as usize) {
                    collect(&n.left, seen);
                    collect(&n.right, seen);
                }
            }
        }
        collect(&self.terms, &mut seen);
        visit(&self.terms, &mut |term| {
            collect(&term.value.postings, &mut seen)
        });
        seen
    }
    fn dirty(&self, generation: u64) -> (usize, usize) {
        let mut count = 0;
        let mut encoded_value_bytes = 0;
        fn postings(
            tree: &Link<u64, Posting>,
            generation: u64,
            count: &mut usize,
            bytes: &mut usize,
        ) {
            if let Some(n) = tree {
                if n.generation == generation {
                    *count += 1;
                    *bytes += 12;
                    postings(&n.left, generation, count, bytes);
                    postings(&n.right, generation, count, bytes);
                }
            }
        }
        fn terms(
            tree: &Link<Arc<str>, Term>,
            generation: u64,
            count: &mut usize,
            bytes: &mut usize,
        ) {
            if let Some(n) = tree {
                if n.generation == generation {
                    *count += 1;
                    *bytes += n.value.name.len() + 8;
                    postings(&n.value.postings, generation, count, bytes);
                    terms(&n.left, generation, count, bytes);
                    terms(&n.right, generation, count, bytes);
                }
            }
        }
        terms(
            &self.terms,
            generation,
            &mut count,
            &mut encoded_value_bytes,
        );
        (count, encoded_value_bytes)
    }
}

struct Document {
    nonempty: bool,
    frequencies: BTreeMap<Arc<str>, u32>,
}
struct Index {
    snapshot: Snapshot,
    documents: BTreeMap<u64, Document>,
    generation: u64,
}
impl Index {
    fn new() -> Self {
        Self {
            snapshot: Snapshot {
                collection: Arc::from("rows"),
                terms: None,
                doc_count: 0,
            },
            documents: BTreeMap::new(),
            generation: 0,
        }
    }
    fn apply(&mut self, changes: &BTreeMap<u64, Option<String>>) -> Work {
        self.generation += 1;
        let mut work = Work::default();
        let mut edits = BTreeMap::<Arc<str>, BTreeMap<u64, Option<u32>>>::new();
        for (&id, text) in changes {
            let previous = self.documents.remove(&id);
            if previous.as_ref().is_some_and(|d| d.nonempty) {
                self.snapshot.doc_count -= 1;
            }
            let mut current = Document {
                nonempty: text.as_ref().is_some_and(|s| !s.trim().is_empty()),
                frequencies: BTreeMap::new(),
            };
            if let Some(text) = text {
                for token in existing_delta::tokenize(text) {
                    *current.frequencies.entry(Arc::from(token)).or_default() += 1;
                }
                self.snapshot.doc_count += u32::from(current.nonempty);
            }
            if let Some(previous) = &previous {
                for (term, &frequency) in &previous.frequencies {
                    if current.frequencies.get(term) != Some(&frequency) {
                        edits.entry(Arc::clone(term)).or_default().insert(id, None);
                    }
                }
            }
            for (term, &frequency) in &current.frequencies {
                if previous.as_ref().and_then(|p| p.frequencies.get(term)) != Some(&frequency) {
                    edits
                        .entry(Arc::clone(term))
                        .or_default()
                        .insert(id, Some(frequency));
                }
            }
            if text.is_some() {
                self.documents.insert(id, current);
            }
        }
        for (term, changes) in edits {
            let previous = find(&self.snapshot.terms, &term, &mut work);
            let mut postings = previous.as_ref().and_then(|n| n.value.postings.clone());
            for (id, frequency) in changes {
                postings = put(
                    &postings,
                    id,
                    frequency.map(|frequency| Posting { id, frequency }),
                    self.generation,
                    &mut work,
                );
            }
            let value = postings
                .is_some()
                .then(|| Term::new(Arc::clone(&term), postings));
            self.snapshot.terms = put(
                &self.snapshot.terms,
                term,
                value,
                self.generation,
                &mut work,
            );
        }
        work
    }
}

fn reference(documents: &BTreeMap<u64, Option<String>>) -> NativeFulltextFrame {
    let mut terms = BTreeMap::<String, Vec<NativeFulltextPosting>>::new();
    let mut doc_count = 0;
    for (&id, text) in documents {
        let Some(text) = text.as_ref().filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        doc_count += 1;
        let mut counts = BTreeMap::<String, u32>::new();
        // Independent full builder, mirroring the native tokenizer contract.
        for token in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() >= 2)
        {
            *counts.entry(token.to_owned()).or_default() += 1;
        }
        for (term, term_count) in counts {
            terms.entry(term).or_default().push(NativeFulltextPosting {
                entity_id: id,
                term_count,
            });
        }
    }
    NativeFulltextFrame {
        collection: "rows".to_owned(),
        doc_count,
        terms: terms
            .into_iter()
            .map(|(term, postings)| NativeFulltextTerm { term, postings })
            .collect(),
    }
}
fn assert_reference(snapshot: &Snapshot, documents: &BTreeMap<u64, Option<String>>) {
    let expected = reddb_file::encode_native_fulltext_frame(&reference(documents));
    assert!(snapshot.bytes() == expected, "exact RDFT bytes differ");
    let digest = snapshot.digest();
    assert_eq!(digest.len, expected.len() as u64);
    assert_eq!(digest.crc, crc32fast::hash(&expected));
    check(&snapshot.terms);
    visit(&snapshot.terms, &mut |n| {
        check(&n.value.postings);
    });
}
fn seed(rows: u64) -> BTreeMap<u64, Option<String>> {
    (0..rows)
        .map(|id| {
            (
                id,
                Some(format!("common seed{id:08} alpha beta gamma delta epsilon")),
            )
        })
        .collect()
}
fn six_versions(rows: u64, cycle: u64) -> BTreeMap<u64, Option<String>> {
    (0..6)
        .map(|id| {
            (
                rows + (cycle - 1) * 6 + id,
                Some(format!(
                    "common edit{cycle:08} row{id:08} alpha beta gamma delta"
                )),
            )
        })
        .collect()
}
fn algorithm_main() {
    if std::env::args().nth(1).as_deref() == Some("baseline") {
        let documents = seed(8192);
        let mut frame = reference(&documents);
        assert_eq!(
            existing_delta::apply_delta(&mut frame, &six_versions(8192, 1), true),
            Some(true)
        );
        let prepared = reddb_file::encode_native_fulltext_frame(&frame);
        println!("{{\"implementation\":\"existing_delta\",\"rows\":8192,\"changed_rows\":6,\"prepared_bytes\":{}}}",prepared.len());
        assert!(
            prepared.len() <= 65536,
            "existing full-frame publication exceeds the experimental 64 KiB changed-value ceiling"
        );
        return;
    }
    for rows in [256, 2048, 8192, 65536] {
        let mut documents = seed(rows);
        let mut index = Index::new();
        index.apply(&documents);
        assert_reference(&index.snapshot, &documents);
        let pinned = index.snapshot.clone();
        let pinned_bytes = pinned.bytes();
        let pinned_nodes = pinned.pointers();
        for cycle in 1..=3 {
            let changes = six_versions(rows, cycle);
            let started = std::time::Instant::now();
            let work = index.apply(&changes);
            let micros = started.elapsed().as_micros();
            let (dirty, dirty_value_bytes) = index.snapshot.dirty(index.generation);
            assert!(dirty_value_bytes <= 65536, "changed values exceed bound");
            assert!(work.visits <= 4096, "tree visits exceed bound");
            documents.extend(changes);
            assert_reference(&index.snapshot, &documents);
            assert!(pinned.bytes() == pinned_bytes, "held root was mutated");
            println!("{{\"rows\":{rows},\"cycle\":{cycle},\"changed_rows\":6,\"visits\":{},\"nodes_created\":{},\"reachable_dirty_nodes\":{dirty},\"dirty_value_bytes\":{dirty_value_bytes},\"canonical_frame_bytes\":{},\"update_microseconds\":{micros},\"held_root_unchanged\":true,\"exact_bytes_and_crc\":true}}",work.visits,work.allocated,index.snapshot.digest().len);
        }
        let current_nodes = index.snapshot.pointers();
        let pinned_only = pinned_nodes.difference(&current_nodes).count();
        assert!(pinned_only > 0, "old snapshot retains replaced nodes");
        // Arc ownership is only an in-memory lifetime demonstration. No claim
        // about native pager retirement or post-crash reclamation is made.
        let old_root = Arc::downgrade(pinned.terms.as_ref().expect("pinned root"));
        drop(pinned);
        assert!(
            old_root.upgrade().is_none(),
            "released old root still pinned"
        );
        println!(
            "{{\"rows\":{rows},\"pinned_only_nodes\":{pinned_only},\"old_root_released\":true}}"
        );
    }
    let mut index = Index::new();
    let mut documents = BTreeMap::new();
    let mut random = 17_u64;
    let samples = [
        None,
        Some(""),
        Some("a"),
        Some("x y"),
        Some("ALPHA alpha café 東京"),
        Some("βeta gamma gamma"),
        Some("\n\t"),
    ];
    for step in 0..200 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let id = (random >> 32) % 100;
        let text = samples[((random >> 16) as usize) % samples.len()].map(str::to_owned);
        let changes = [(id, text)].into_iter().collect();
        index.apply(&changes);
        documents.extend(changes);
        assert_reference(&index.snapshot, &documents);
        if step % 7 == 0 {
            let pinned = index.snapshot.clone();
            let expected = pinned.bytes();
            index.apply(&BTreeMap::new());
            assert!(pinned.bytes() == expected);
        }
    }
    println!("{{\"differential_steps\":200,\"insert_update_delete_unicode_tokenless_empty\":true,\"exact_bytes_and_crc\":true}}");
}

static NEXT_NODE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static DROPPED_NODE_IDS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
impl<K, V> Drop for Node<K, V> {
    fn drop(&mut self) {
        DROPPED_NODE_IDS
            .lock()
            .expect("retirement queue")
            .push(self.id);
    }
}

mod disk {
    use super::*;
    use reddb_engine::storage::page_store::PageStore;
    use reddb_server::storage::engine::pager::PagerError;
    use reddb_server::storage::engine::{
        Page, PageType, Pager, PagerConfig, HEADER_SIZE, PAGE_SIZE,
    };
    use reddb_server::storage::wal::{Checkpointer, TransactionManager};
    use std::collections::BTreeSet;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct BufferedPages {
        pager: Arc<Pager>,
        pages: Mutex<BTreeMap<u32, Page>>,
        freed: Mutex<BTreeSet<u32>>,
    }
    impl PageStore for BufferedPages {
        fn read_page(&self, id: u32) -> Result<Page, PagerError> {
            if let Some(page) = self.pages.lock().expect("page buffer").get(&id).cloned() {
                return Ok(page);
            }
            self.pager.read_page(id)
        }
        fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
            self.pages.lock().expect("page buffer").insert(id, page);
            Ok(())
        }
        fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
            let page = self.pager.allocate_page(kind)?;
            self.pages
                .lock()
                .expect("page buffer")
                .insert(page.page_id(), page.clone());
            Ok(page)
        }
        fn free_page(&self, id: u32) -> Result<(), PagerError> {
            self.freed.lock().expect("deferred frees").insert(id);
            Ok(())
        }
        fn page_count(&self) -> Result<u32, PagerError> {
            self.pager.page_count()
        }
    }
    type PackedTree = reddb_engine::storage::btree::BTree<BufferedPages>;
    fn node_key(id: u64) -> Vec<u8> {
        let mut key = vec![b'N'];
        key.extend_from_slice(&id.to_be_bytes());
        key
    }
    fn doc_key(id: u64) -> Vec<u8> {
        let mut key = vec![b'D'];
        key.extend_from_slice(&id.to_be_bytes());
        key
    }
    fn u32bytes(out: &mut Vec<u8>, n: u32) {
        out.extend_from_slice(&n.to_le_bytes());
    }
    fn u64bytes(out: &mut Vec<u8>, n: u64) {
        out.extend_from_slice(&n.to_le_bytes());
    }
    fn string(out: &mut Vec<u8>, s: &str) {
        u32bytes(out, s.len() as u32);
        out.extend_from_slice(s.as_bytes());
    }
    fn root_id<K, V>(tree: &Link<K, V>) -> u64 {
        tree.as_ref().map_or(0, |n| n.id)
    }
    struct Reader<'a> {
        bytes: &'a [u8],
        pos: usize,
    }
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> &'a [u8] {
            let out = self
                .bytes
                .get(self.pos..self.pos + n)
                .expect("bounded record");
            self.pos += n;
            out
        }
        fn u8(&mut self) -> u8 {
            self.take(1)[0]
        }
        fn u32(&mut self) -> u32 {
            u32::from_le_bytes(self.take(4).try_into().expect("u32"))
        }
        fn u64(&mut self) -> u64 {
            u64::from_le_bytes(self.take(8).try_into().expect("u64"))
        }
        fn string(&mut self) -> String {
            let n = self.u32() as usize;
            String::from_utf8(self.take(n).to_vec()).expect("UTF8")
        }
        fn finish(&self) {
            assert_eq!(self.pos, self.bytes.len(), "trailing record bytes");
        }
    }
    struct SavedNode {
        kind: u8,
        id: u64,
        generation: u64,
        height: u32,
        entries: u32,
        digest: Summary,
        left: u64,
        right: u64,
        name: String,
        postings: u64,
        entity: u64,
        frequency: u32,
    }
    fn encode_node<K, V>(n: &Node<K, V>, kind: u8) -> Vec<u8> {
        let mut out = b"NFP0".to_vec();
        out.push(kind);
        u64bytes(&mut out, n.id);
        u64bytes(&mut out, n.generation);
        u32bytes(&mut out, n.height);
        u32bytes(&mut out, n.entries);
        u64bytes(&mut out, n.summary.len);
        u32bytes(&mut out, n.summary.crc);
        u64bytes(&mut out, root_id(&n.left));
        u64bytes(&mut out, root_id(&n.right));
        out
    }
    fn decode_node(bytes: &[u8]) -> SavedNode {
        let mut r = Reader { bytes, pos: 0 };
        assert_eq!(r.take(4), b"NFP0");
        let kind = r.u8();
        let mut out = SavedNode {
            kind,
            id: r.u64(),
            generation: r.u64(),
            height: r.u32(),
            entries: r.u32(),
            digest: Summary {
                len: r.u64(),
                crc: r.u32(),
            },
            left: r.u64(),
            right: r.u64(),
            name: String::new(),
            postings: 0,
            entity: 0,
            frequency: 0,
        };
        if kind == 1 {
            out.entity = r.u64();
            out.frequency = r.u32();
        } else {
            assert_eq!(kind, 2);
            out.name = r.string();
            out.postings = r.u64();
        }
        r.finish();
        out
    }
    #[derive(Clone)]
    struct Root {
        tree: u32,
        logical: u64,
        docs: u32,
        digest: Summary,
        next_id: u64,
        generation: u64,
    }
    impl Root {
        fn encode(&self) -> Vec<u8> {
            let mut out = b"NFPM".to_vec();
            u32bytes(&mut out, self.tree);
            u64bytes(&mut out, self.logical);
            u32bytes(&mut out, self.docs);
            u64bytes(&mut out, self.digest.len);
            u32bytes(&mut out, self.digest.crc);
            u64bytes(&mut out, self.next_id);
            u64bytes(&mut out, self.generation);
            out
        }
        fn decode(bytes: &[u8]) -> Self {
            let mut r = Reader { bytes, pos: 0 };
            assert_eq!(r.take(4), b"NFPM");
            Self {
                tree: r.u32(),
                logical: r.u64(),
                docs: r.u32(),
                digest: Summary {
                    len: r.u64(),
                    crc: r.u32(),
                },
                next_id: r.u64(),
                generation: r.u64(),
            }
        }
    }
    struct Store {
        pages: Arc<BufferedPages>,
        tree: PackedTree,
        manifest: u32,
        wal: PathBuf,
        known: HashSet<u64>,
        recovered: u64,
    }
    impl Store {
        fn open(directory: &Path) -> Self {
            let path = directory.join("parts.rdb");
            let wal = directory.join("parts.wal");
            let pager = Arc::new(
                Pager::open(
                    &path,
                    PagerConfig {
                        cache_size: 512,
                        ..PagerConfig::default()
                    },
                )
                .expect("native pager"),
            );
            let recovered = if wal.exists() {
                Checkpointer::recover(&pager, &wal)
                    .expect("native WAL recovery")
                    .pages_checkpointed
            } else {
                0
            };
            let mut header = pager.physical_header().expect("header");
            let (manifest, root) = if header.vector_artifact_page != 0 {
                let page = pager
                    .read_page(header.vector_artifact_page)
                    .expect("probe manifest");
                let root = Root::decode(&page.as_bytes()[HEADER_SIZE..]);
                NEXT_NODE_ID.fetch_max(root.next_id, std::sync::atomic::Ordering::Relaxed);
                (header.vector_artifact_page, root.tree)
            } else {
                let page = pager
                    .allocate_page(PageType::NativeMeta)
                    .expect("manifest allocation");
                header.vector_artifact_page = page.page_id();
                pager
                    .update_physical_header(header)
                    .expect("fixed manifest pointer");
                pager.flush().expect("initial allocation publication");
                (page.page_id(), 0)
            };
            let pages = Arc::new(BufferedPages {
                pager,
                pages: Mutex::new(BTreeMap::new()),
                freed: Mutex::new(BTreeSet::new()),
            });
            let tree = PackedTree::with_root(Arc::clone(&pages), root);
            Self {
                pages,
                tree,
                manifest,
                wal,
                known: HashSet::new(),
                recovered,
            }
        }
        fn saved(&self, id: u64) -> SavedNode {
            let bytes = self
                .tree
                .get(&node_key(id))
                .expect("packed lookup")
                .expect("reachable node exists");
            let n = decode_node(&bytes);
            assert_eq!(n.id, id);
            n
        }
        fn save_postings(
            &mut self,
            tree: &Link<u64, Posting>,
            generation: u64,
            out: &mut BTreeMap<Vec<u8>, Vec<u8>>,
        ) {
            if let Some(n) = tree {
                if n.generation != generation {
                    return;
                }
                self.save_postings(&n.left, generation, out);
                self.save_postings(&n.right, generation, out);
                if self.known.insert(n.id) {
                    let mut bytes = encode_node(n, 1);
                    u64bytes(&mut bytes, n.value.id);
                    u32bytes(&mut bytes, n.value.frequency);
                    out.insert(node_key(n.id), bytes);
                }
            }
        }
        fn save_terms(
            &mut self,
            tree: &Link<Arc<str>, Term>,
            generation: u64,
            out: &mut BTreeMap<Vec<u8>, Vec<u8>>,
        ) {
            if let Some(n) = tree {
                if n.generation != generation {
                    return;
                }
                self.save_terms(&n.left, generation, out);
                self.save_terms(&n.right, generation, out);
                self.save_postings(&n.value.postings, generation, out);
                if self.known.insert(n.id) {
                    let mut bytes = encode_node(n, 2);
                    string(&mut bytes, &n.value.name);
                    u64bytes(&mut bytes, root_id(&n.value.postings));
                    out.insert(node_key(n.id), bytes);
                }
            }
        }
        fn persist(
            &mut self,
            index: &Index,
            changed: &[u64],
            crash: Option<&str>,
        ) -> serde_json::Value {
            let mut records = BTreeMap::new();
            self.save_terms(&index.snapshot.terms, index.generation, &mut records);
            let node_records = records.len();
            let mut inverse_bytes = 0;
            for &id in changed {
                if let Some(doc) = index.documents.get(&id) {
                    let mut b = vec![u8::from(doc.nonempty)];
                    u32bytes(&mut b, doc.frequencies.len() as u32);
                    for (term, count) in &doc.frequencies {
                        string(&mut b, term);
                        u32bytes(&mut b, *count);
                    }
                    inverse_bytes += b.len();
                    records.insert(doc_key(id), b);
                } else {
                    self.tree.delete(&doc_key(id)).expect("inverse delete");
                }
            }
            let record_bytes = records
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>();
            self.tree
                .upsert_batch_sorted(&records.into_iter().collect::<Vec<_>>())
                .expect("packed node batch");
            let retired = {
                let mut queue = DROPPED_NODE_IDS.lock().expect("retirement queue");
                let n = queue.len().min(4096);
                queue.drain(..n).collect::<Vec<_>>()
            };
            let scanned = retired.len();
            let mut deleted = 0;
            for id in retired {
                if self.known.remove(&id) {
                    assert!(self
                        .tree
                        .delete(&node_key(id))
                        .expect("retired record deletion"));
                    deleted += 1;
                }
            }
            let root = Root {
                tree: self.tree.root_page_id(),
                logical: root_id(&index.snapshot.terms),
                docs: index.snapshot.doc_count,
                digest: index.snapshot.digest(),
                next_id: NEXT_NODE_ID.load(std::sync::atomic::Ordering::Relaxed),
                generation: index.generation,
            };
            let mut manifest = Page::new(PageType::NativeMeta, self.manifest);
            let payload = root.encode();
            manifest.as_bytes_mut()[HEADER_SIZE..HEADER_SIZE + payload.len()]
                .copy_from_slice(&payload);
            self.pages
                .write_page(self.manifest, manifest)
                .expect("buffer manifest");
            let pending = std::mem::take(&mut *self.pages.pages.lock().expect("pending pages"));
            let count = pending.len();
            // Publish allocation/header reservations while every changed old
            // page remains private in the overlay. Reserved blanks are not
            // reachable through the committed logical root.
            self.pages
                .pager
                .flush()
                .expect("allocation reservation flush");
            let manager = Arc::new(
                TransactionManager::new(Arc::clone(&self.pages.pager), &self.wal)
                    .expect("native transaction manager"),
            );
            let mut tx = manager.begin().expect("begin native page transaction");
            for (&id, page) in &pending {
                tx.write_page(id, page.clone()).expect("stage page");
            }
            if crash == Some("before_commit") {
                manager.sync_wal().expect("durable begin only");
                std::process::exit(72);
            }
            tx.commit().expect("durable native page commit");
            if crash == Some("after_commit") {
                std::process::exit(73);
            }
            if crash == Some("partial_home") {
                let (&id, page) = pending
                    .iter()
                    .find(|(id, _)| **id != self.manifest)
                    .expect("data page for modeled partial apply");
                let mut page = page.clone();
                page.update_checksum();
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.pages.pager.path())
                    .expect("fault-model home file");
                file.write_all_at(page.as_bytes(), id as u64 * PAGE_SIZE as u64)
                    .expect("one complete home image");
                file.sync_all().expect("sync modeled single home write");
                std::process::exit(74);
            }
            let wal_before_drain = std::fs::metadata(&self.wal).expect("WAL metadata").len();
            drop(manager);
            let frees = std::mem::take(&mut *self.pages.freed.lock().expect("deferred frees"));
            for &id in &frees {
                self.pages.pager.free_page(id).expect("post-commit free");
            }
            self.pages.pager.flush().expect("protected page flush");
            Checkpointer::recover(&self.pages.pager, &self.wal).expect("native WAL drain");
            assert!(
                count <= 32 || changed.len() > 6,
                "six-edit page transaction exceeds experimental 32-page ceiling"
            );
            let meta = std::fs::metadata(self.pages.pager.path()).expect("main allocation");
            let wal = std::fs::metadata(&self.wal).expect("WAL allocation");
            serde_json::json!({"changed_documents":changed.len(),"new_node_records":node_records,"record_key_value_bytes":record_bytes,"inverse_bytes":inverse_bytes,"transaction_pages":count,"transaction_page_image_bytes":count*PAGE_SIZE,"retirement_ids_scanned":scanned,"retired_records_deleted":deleted,"physical_pages_freed":frees.len(),"main_logical_bytes":meta.len(),"main_allocated_bytes":meta.blocks()*512,"free_pages":self.pages.pager.free_page_count().expect("free pages"),"wal_before_drain_bytes":wal_before_drain,"wal_after_drain_bytes":wal.len(),"wal_allocated_bytes":wal.blocks()*512})
        }
        fn root(&self) -> Root {
            let page = self.pages.pager.read_page(self.manifest).expect("manifest");
            Root::decode(&page.as_bytes()[HEADER_SIZE..])
        }
        fn postings_bytes(&self, id: u64, seen: &mut HashSet<u64>) -> Vec<u8> {
            if id == 0 {
                return Vec::new();
            }
            assert!(seen.insert(id), "acyclic ownership");
            let n = self.saved(id);
            assert_eq!(n.kind, 1);
            let mut bytes = self.postings_bytes(n.left, seen);
            bytes.extend_from_slice(
                &Posting {
                    id: n.entity,
                    frequency: n.frequency,
                }
                .bytes(),
            );
            bytes.extend(self.postings_bytes(n.right, seen));
            assert_eq!(n.digest.len, bytes.len() as u64);
            assert_eq!(n.digest.crc, crc32fast::hash(&bytes));
            bytes
        }
        fn terms_bytes(&self, id: u64, seen: &mut HashSet<u64>) -> Vec<u8> {
            if id == 0 {
                return Vec::new();
            }
            assert!(seen.insert(id), "acyclic ownership");
            let n = self.saved(id);
            assert_eq!(n.kind, 2);
            let mut bytes = self.terms_bytes(n.left, seen);
            string(&mut bytes, &n.name);
            let postings = self.postings_bytes(n.postings, seen);
            u32bytes(&mut bytes, (postings.len() / 12) as u32);
            bytes.extend(postings);
            bytes.extend(self.terms_bytes(n.right, seen));
            assert_eq!(n.digest.len, bytes.len() as u64);
            assert_eq!(n.digest.crc, crc32fast::hash(&bytes));
            bytes
        }
        fn render(&self, root: &Root) -> Vec<u8> {
            let mut out = b"RDFT".to_vec();
            string(&mut out, "rows");
            u32bytes(&mut out, root.docs);
            let terms = if root.logical == 0 {
                0
            } else {
                self.saved(root.logical).entries
            };
            u32bytes(&mut out, terms);
            out.extend(self.terms_bytes(root.logical, &mut HashSet::new()));
            assert_eq!(root.digest.len, out.len() as u64);
            assert_eq!(root.digest.crc, crc32fast::hash(&out));
            out
        }
        fn load_postings(&mut self, id: u64) -> Link<u64, Posting> {
            if id == 0 {
                return None;
            }
            let n = self.saved(id);
            assert_eq!(n.kind, 1);
            self.known.insert(id);
            Some(Arc::new(Node {
                id,
                key: n.entity,
                value: Posting {
                    id: n.entity,
                    frequency: n.frequency,
                },
                left: self.load_postings(n.left),
                right: self.load_postings(n.right),
                height: n.height,
                entries: n.entries,
                summary: n.digest,
                generation: n.generation,
            }))
        }
        fn load_terms(&mut self, id: u64) -> Link<Arc<str>, Term> {
            if id == 0 {
                return None;
            }
            let n = self.saved(id);
            assert_eq!(n.kind, 2);
            self.known.insert(id);
            let name: Arc<str> = Arc::from(n.name);
            let value = Term::new(Arc::clone(&name), self.load_postings(n.postings));
            Some(Arc::new(Node {
                id,
                key: name,
                value,
                left: self.load_terms(n.left),
                right: self.load_terms(n.right),
                height: n.height,
                entries: n.entries,
                summary: n.digest,
                generation: n.generation,
            }))
        }
        fn load_doc(&self, id: u64) -> Document {
            let bytes = self
                .tree
                .get(&doc_key(id))
                .expect("inverse lookup")
                .expect("inverse record");
            let mut r = Reader {
                bytes: &bytes,
                pos: 0,
            };
            let nonempty = r.u8() != 0;
            let mut frequencies = BTreeMap::new();
            for _ in 0..r.u32() {
                let term = Arc::from(r.string());
                let count = r.u32();
                frequencies.insert(term, count);
            }
            r.finish();
            Document {
                nonempty,
                frequencies,
            }
        }
    }
    fn changes(cycle: u64) -> BTreeMap<u64, Option<String>> {
        (0..6)
            .map(|id| {
                (
                    id,
                    Some(format!(
                        "common edit{cycle:08} row{id:08} alpha beta gamma delta"
                    )),
                )
            })
            .collect()
    }
    fn expected(rows: u64, cycle: u64) -> Vec<u8> {
        let mut docs = seed(rows);
        if cycle > 0 {
            docs.extend(changes(cycle));
        }
        reddb_file::encode_native_fulltext_frame(&reference(&docs))
    }

    pub fn main() {
        let args = std::env::args().collect::<Vec<_>>();
        if args.get(1).map(String::as_str) == Some("crash-child") {
            let directory = Path::new(&args[2]);
            let mode = &args[3];
            let mut store = Store::open(directory);
            let root = store.root();
            let terms = store.load_terms(root.logical);
            let documents = (0..6).map(|id| (id, store.load_doc(id))).collect();
            let mut index = Index {
                snapshot: Snapshot {
                    collection: Arc::from("rows"),
                    terms,
                    doc_count: root.docs,
                },
                documents,
                generation: root.generation,
            };
            DROPPED_NODE_IDS
                .lock()
                .expect("empty restored queue")
                .clear();
            index.apply(&changes(13));
            store.persist(&index, &(0..6).collect::<Vec<_>>(), Some(mode));
            panic!("crash point not reached");
        }
        let root = PathBuf::from(args.get(1).expect("fresh fixture directory"));
        std::fs::create_dir(&root).expect("fresh synthetic root");
        for rows in [256, 2048, 8192] {
            let directory = root.join(rows.to_string());
            std::fs::create_dir(&directory).expect("fresh size fixture");
            let mut store = Store::open(&directory);
            let mut index = Index::new();
            index.apply(&seed(rows));
            DROPPED_NODE_IDS
                .lock()
                .expect("unpersisted seed drops")
                .clear();
            let initial = store.persist(&index, &(0..rows).collect::<Vec<_>>(), None);
            assert!(store.render(&store.root()) == expected(rows, 0));
            println!(
                "{}",
                serde_json::json!({"rows":rows,"cycle":0,"metrics":initial,"exact_bytes_crc":true})
            );
            let mut pinned = Some(index.snapshot.clone());
            let old_root = store.root();
            let old_bytes = store.render(&old_root);
            for cycle in 1..=12 {
                index.apply(&changes(cycle));
                let metrics = store.persist(&index, &(0..6).collect::<Vec<_>>(), None);
                assert!(store.render(&store.root()) == expected(rows, cycle));
                if pinned.is_some() {
                    assert!(
                        store.render(&old_root) == old_bytes,
                        "held disk root changed or reclaimed"
                    );
                }
                println!(
                    "{}",
                    serde_json::json!({"rows":rows,"cycle":cycle,"metrics":metrics,"exact_bytes_crc":true,"old_root_pinned":pinned.is_some()})
                );
                if cycle == 3 {
                    drop(pinned.take());
                    let metrics = store.persist(&index, &[], None);
                    assert!(store.render(&store.root()) == expected(rows, cycle));
                    println!(
                        "{}",
                        serde_json::json!({"rows":rows,"released_pin":true,"metrics":metrics})
                    );
                }
            }
            drop(index);
            drop(store);
            let store = Store::open(&directory);
            assert!(store.render(&store.root()) == expected(rows, 12));
            for id in 0..6 {
                let doc = store.load_doc(id);
                assert!(doc.nonempty);
                assert!(doc.frequencies.contains_key("edit00000012"));
            }
            drop(store);
            println!(
                "{}",
                serde_json::json!({"rows":rows,"cold_reopen_and_inverse":true})
            );
        }
        let source = root.join("8192");
        let originals = ["parts.rdb", "parts.wal"]
            .map(|name| std::fs::read(source.join(name)).expect("frozen source bytes"));
        for (mode, exit_code, cycle) in [
            ("before_commit", 72, 12),
            ("after_commit", 73, 13),
            ("partial_home", 74, 13),
        ] {
            let directory = root.join(format!("crash-{mode}"));
            std::fs::create_dir(&directory).expect("fresh crash copy");
            for name in ["parts.rdb", "parts.wal"] {
                std::fs::copy(source.join(name), directory.join(name))
                    .expect("copy stopped fixture");
            }
            let status =
                std::process::Command::new(std::env::current_exe().expect("probe executable"))
                    .args([
                        "crash-child",
                        directory.to_str().expect("fixture path"),
                        mode,
                    ])
                    .status()
                    .expect("crash child");
            assert_eq!(status.code(), Some(exit_code));
            let store = Store::open(&directory);
            assert!(
                store.render(&store.root()) == expected(8192, cycle),
                "recovery frame differs"
            );
            for id in 0..6 {
                assert!(store
                    .load_doc(id)
                    .frequencies
                    .contains_key(format!("edit{cycle:08}").as_str()));
            }
            println!(
                "{}",
                serde_json::json!({"crash":mode,"expected_exit":exit_code,"recovery_pages":store.recovered,"exact_bytes_crc_and_inverse":true})
            );
        }
        for (i, name) in ["parts.rdb", "parts.wal"].iter().enumerate() {
            assert!(
                std::fs::read(source.join(name)).expect("source preservation") == originals[i],
                "frozen source changed"
            );
        }
        println!("{}", serde_json::json!({"frozen_source_preserved":true}));
    }
}
fn main() {
    disk::main();
}
