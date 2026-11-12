//! Cluster multi-writer runtime composition drill.
//!
//! The sharded cluster runtime is not yet transport-wired end to end. This test
//! exercises the current integration boundary without pretending otherwise:
//! the real ownership catalog admits concurrent writers for disjoint ranges,
//! fences a stale owner after transfer, and the admitted writes cross the real
//! persistent runtime/checkpoint/reopen path on the owning data members.

use std::path::Path;
use std::sync::{Arc, Barrier};

use reddb_server::cluster::{
    CollectionId, NodeIdentity, OwnershipEpoch, PlacementMetadata, RangeBound, RangeBounds,
    RangeId, RangeOwnership, ShardKeyMode, ShardOwnershipCatalog,
};
use reddb_server::{RedDBOptions, RedDBRuntime};

const NODE_A: &str = "CN=cluster-a,O=reddb";
const NODE_B: &str = "CN=cluster-b,O=reddb";
const KEY_LOW: [u8; 1] = [0x10];
const KEY_HIGH: [u8; 1] = [0x90];

fn ident(subject: &str) -> NodeIdentity {
    NodeIdentity::from_certificate_subject(subject).expect("valid node identity")
}

fn range(
    collection: &CollectionId,
    id: u64,
    bounds: RangeBounds,
    owner: &str,
    replica: &str,
) -> RangeOwnership {
    RangeOwnership::establish(
        collection.clone(),
        RangeId::new(id),
        ShardKeyMode::Ordered,
        bounds,
        ident(owner),
        [ident(replica)],
        PlacementMetadata::with_replication_factor(2),
    )
}

fn cluster_catalog() -> (ShardOwnershipCatalog, CollectionId) {
    let collection = CollectionId::new("cluster_writes").expect("valid collection");
    let mut catalog = ShardOwnershipCatalog::new();
    catalog
        .declare_collection(collection.clone(), ShardKeyMode::Ordered)
        .expect("declare ordered collection");
    catalog
        .apply_update(range(
            &collection,
            1,
            RangeBounds::new(RangeBound::Min, RangeBound::key([0x80])).expect("lower range"),
            NODE_A,
            NODE_B,
        ))
        .expect("install lower range");
    catalog
        .apply_update(range(
            &collection,
            2,
            RangeBounds::new(RangeBound::key([0x80]), RangeBound::Max).expect("upper range"),
            NODE_B,
            NODE_A,
        ))
        .expect("install upper range");
    (catalog, collection)
}

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .expect("cluster member runtime boots")
}

fn rows(runtime: &RedDBRuntime) -> usize {
    runtime
        .execute_query("SELECT * FROM cluster_writes")
        .expect("cluster shard rows are readable")
        .result
        .records
        .len()
}

#[derive(Debug)]
struct RoutedWrite {
    admission: Result<(), String>,
    write: Option<Result<(), String>>,
}

fn spawn_routed_write(
    catalog: Arc<ShardOwnershipCatalog>,
    collection: CollectionId,
    node: NodeIdentity,
    key: Vec<u8>,
    epoch: OwnershipEpoch,
    runtime: RedDBRuntime,
    sql: &'static str,
    start: Arc<Barrier>,
) -> std::thread::JoinHandle<RoutedWrite> {
    std::thread::spawn(move || {
        let admission = catalog
            .admit_public_write(&node, &collection, &key, epoch)
            .map(|_| ())
            .map_err(|err| err.to_string());
        start.wait();
        let write = admission.as_ref().ok().map(|_| {
            runtime
                .execute_query(sql)
                .map(|_| ())
                .map_err(|err| err.to_string())
        });
        RoutedWrite { admission, write }
    })
}

#[test]
fn disjoint_range_writers_commit_concurrently_and_stale_owner_is_fenced() {
    let root = tempfile::tempdir().expect("private cluster drill directory");
    let path_a = root.path().join("node-a").join("data.rdb");
    let path_b = root.path().join("node-b").join("data.rdb");
    let runtime_a = open(&path_a);
    let runtime_b = open(&path_b);
    let (catalog, collection) = cluster_catalog();
    let catalog = Arc::new(catalog);

    let start = Arc::new(Barrier::new(2));
    let lower = spawn_routed_write(
        catalog.clone(),
        collection.clone(),
        ident(NODE_A),
        KEY_LOW.to_vec(),
        OwnershipEpoch::initial(),
        runtime_a.clone(),
        "INSERT INTO cluster_writes (id, owner) VALUES (16, 'cluster-a')",
        start.clone(),
    );
    let upper = spawn_routed_write(
        catalog.clone(),
        collection.clone(),
        ident(NODE_B),
        KEY_HIGH.to_vec(),
        OwnershipEpoch::initial(),
        runtime_b.clone(),
        "INSERT INTO cluster_writes (id, owner) VALUES (144, 'cluster-b')",
        start,
    );
    for result in [
        lower.join().expect("lower writer does not panic"),
        upper.join().expect("upper writer does not panic"),
    ] {
        assert!(
            result.admission.is_ok(),
            "owner admission failed: {result:?}"
        );
        assert!(
            matches!(result.write, Some(Ok(()))),
            "admitted runtime write failed: {result:?}"
        );
    }

    let mut transferred = (*catalog).clone();
    let before = transferred
        .range(&collection, RangeId::new(1))
        .expect("lower range exists")
        .clone();
    let old_epoch = before.epoch();
    transferred
        .apply_update(before.transfer_to(ident(NODE_B), [ident(NODE_A)]))
        .expect("transfer lower range to node B");
    let new_epoch = transferred
        .range(&collection, RangeId::new(1))
        .expect("transferred lower range exists")
        .epoch();
    let transferred = Arc::new(transferred);

    let start = Arc::new(Barrier::new(2));
    let stale = spawn_routed_write(
        transferred.clone(),
        collection.clone(),
        ident(NODE_A),
        KEY_LOW.to_vec(),
        old_epoch,
        runtime_a.clone(),
        "INSERT INTO cluster_writes (id, owner) VALUES (17, 'stale-cluster-a')",
        start.clone(),
    );
    let current = spawn_routed_write(
        transferred,
        collection,
        ident(NODE_B),
        KEY_LOW.to_vec(),
        new_epoch,
        runtime_b.clone(),
        "INSERT INTO cluster_writes (id, owner) VALUES (17, 'cluster-b-after-transfer')",
        start,
    );
    let stale = stale.join().expect("stale writer does not panic");
    let current = current.join().expect("current writer does not panic");
    assert!(
        stale.admission.is_err(),
        "old owner was not fenced: {stale:?}"
    );
    assert!(
        stale.write.is_none(),
        "a fenced owner must never reach the storage engine: {stale:?}"
    );
    assert!(
        current.admission.is_ok(),
        "new owner was not admitted: {current:?}"
    );
    assert!(
        matches!(current.write, Some(Ok(()))),
        "new owner write failed: {current:?}"
    );

    runtime_a.checkpoint().expect("checkpoint node A shard");
    runtime_b.checkpoint().expect("checkpoint node B shards");
    drop(runtime_a);
    drop(runtime_b);

    let reopened_a = open(&path_a);
    let reopened_b = open(&path_b);
    assert_eq!(
        rows(&reopened_a),
        1,
        "node A keeps its pre-transfer commit only"
    );
    assert_eq!(
        rows(&reopened_b),
        2,
        "node B keeps its original range commit and post-transfer commit"
    );
}
