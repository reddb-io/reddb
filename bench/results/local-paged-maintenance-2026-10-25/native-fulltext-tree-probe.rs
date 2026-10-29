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
fn main() {
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
