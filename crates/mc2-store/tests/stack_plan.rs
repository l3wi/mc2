//! Stack-plan contract.
//!
//! Committing an apply plan must mean the same thing on both backends, so every
//! scenario below runs against [`MemoryStore`] and a temp-dir [`SqliteStore`]
//! (D5). The two failure scenarios are SQLite-only: they inject a real
//! constraint failure (a `RAISE(ABORT)` trigger) to prove the commit is one
//! transaction.

use mc2_store::{InstanceRecord, InstanceSshRecord, MemoryStore, SqliteStore, StackPlan, Store};
use std::sync::Arc;
use tempfile::TempDir;

async fn memory() -> Arc<dyn Store> {
    let store = MemoryStore::new();
    store.init_cluster("").await.unwrap();
    store
}

async fn sqlite() -> (Arc<dyn Store>, TempDir) {
    let (store, dir) = sqlite_concrete().await;
    let store: Arc<dyn Store> = store;
    (store, dir)
}

async fn sqlite_concrete() -> (Arc<SqliteStore>, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("mc2.db")).await.unwrap();
    store.init_cluster("").await.unwrap();
    (store, dir)
}

fn spec(image: &str) -> String {
    format!(r#"{{"image":"{image}"}}"#)
}

async fn instances(store: &Arc<dyn Store>, stack: &str, service: &str) -> Vec<InstanceRecord> {
    store
        .list_instances()
        .await
        .unwrap()
        .into_iter()
        .filter(|i| i.stack == stack && i.service == service)
        .collect()
}

fn ssh_row(instance_id: &str) -> InstanceSshRecord {
    InstanceSshRecord {
        instance_id: instance_id.to_string(),
        desired: true,
        ..Default::default()
    }
}

/// The plan is the *whole* desired state: replicas are created, matched in place
/// by `(service, ordinal)`, and everything the plan does not mention is dropped —
/// together with the ssh/network rows hanging off it.
async fn creates_updates_and_prunes(store: Arc<dyn Store>) {
    let commit = store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "name: shop\n",
            vec![
                ("db", vec![spec("postgres"), spec("postgres")]),
                ("worker", vec![spec("alpine")]),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(commit.len(), 3, "returns the committed instances");

    let db_before = instances(&store, "shop", "db").await;
    assert_eq!(db_before.len(), 2);
    assert_eq!(
        db_before.iter().map(|i| i.ordinal).collect::<Vec<_>>(),
        vec![0, 1]
    );
    let worker = instances(&store, "shop", "worker").await.remove(0);
    store
        .update_instance_network_observed(&worker.id, "Ready", "{}", None)
        .await
        .unwrap();
    store
        .put_instance_ssh_desired(&ssh_row(&worker.id))
        .await
        .unwrap();

    // The next plan drops `worker` and scales `db` down to one replica.
    let commit = store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "name: shop\n",
            vec![("db", vec![spec("postgres:16")])],
        ))
        .await
        .unwrap();
    assert_eq!(commit.len(), 1);

    let db_after = instances(&store, "shop", "db").await;
    assert_eq!(db_after.len(), 1);
    assert_eq!(
        db_after[0].id, db_before[0].id,
        "ordinal 0 is updated in place"
    );
    assert_eq!(db_after[0].ordinal, 0);
    assert_eq!(db_after[0].spec_json, spec("postgres:16"));

    assert!(instances(&store, "shop", "worker").await.is_empty());
    assert!(store.get_instance(&worker.id).await.unwrap().is_none());
    assert!(
        store
            .get_instance_network(&worker.id)
            .await
            .unwrap()
            .is_none(),
        "network row cascades with the instance"
    );
    assert!(
        !store
            .list_instance_ssh()
            .await
            .unwrap()
            .iter()
            .any(|r| r.instance_id == worker.id),
        "ssh row cascades with the instance"
    );
    assert!(store.get_stack("shop").await.unwrap().is_some());
}

/// An update touches only the spec: placement, runtime id, observed phase,
/// message and health survive, so the node — not the apply — decides recreate.
async fn update_preserves_placement_and_observed_state(store: Arc<dyn Store>) {
    store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "yaml",
            vec![("web", vec![spec("alpine")])],
        ))
        .await
        .unwrap();
    let id = instances(&store, "shop", "web").await.remove(0).id;
    store.bind_instance_to_node(&id, "node-a").await.unwrap();
    store
        .update_instance_status(&id, "Running", Some("rt-1"), Some("up"))
        .await
        .unwrap();
    store.update_instance_health(&id, true).await.unwrap();
    // The applied-config hash records what the node has actually created; a new
    // spec must leave it stale (not clear it), so the node sees the change (B3).
    store
        .set_instance_applied_hash(&id, Some("applied-v1"))
        .await
        .unwrap();

    store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "yaml",
            vec![("web", vec![spec("alpine:3.20")])],
        ))
        .await
        .unwrap();

    let row = store.get_instance(&id).await.unwrap().expect("same row");
    assert_eq!(row.spec_json, spec("alpine:3.20"));
    assert_eq!(row.node_id.as_deref(), Some("node-a"));
    assert_eq!(row.runtime_id.as_deref(), Some("rt-1"));
    assert_eq!(row.phase, "Running");
    assert_eq!(row.message.as_deref(), Some("up"));
    assert!(row.healthy);
    assert_eq!(row.applied_hash.as_deref(), Some("applied-v1"));
}

/// A failing statement inside the commit rolls the whole plan back: the stack
/// row, every update and every delete are undone, and the previous state is
/// untouched.
async fn failed_commit_rolls_back(store: Arc<dyn Store>, pool: &sqlx::SqlitePool) {
    store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "old",
            vec![("web", vec![spec("alpine")]), ("old", vec![spec("alpine")])],
        ))
        .await
        .unwrap();
    let web_id = instances(&store, "shop", "web").await.remove(0).id;

    sqlx::query(
        "CREATE TRIGGER boom BEFORE INSERT ON instances BEGIN SELECT RAISE(ABORT, 'boom'); END",
    )
    .execute(pool)
    .await
    .unwrap();

    // The stack row and `web`'s updated spec are written before the failing
    // INSERT; dropping the `old` service is a DELETE before it too.
    let plan = StackPlan::replicas(
        "shop",
        "{}",
        "new",
        vec![("web", vec![spec("beta"), spec("gamma")])],
    );
    assert!(store.commit_stack_plan(&plan).await.is_err());

    let stack = store.get_stack("shop").await.unwrap().unwrap();
    assert_eq!(stack.raw_yaml, "old");
    let rows = store.list_instances().await.unwrap();
    assert_eq!(rows.len(), 2, "the pruned instance came back: {rows:?}");
    let web = instances(&store, "shop", "web").await;
    assert_eq!(web[0].id, web_id);
    assert_eq!(web[0].spec_json, spec("alpine"));
    assert_eq!(instances(&store, "shop", "old").await.len(), 1);
}

/// `delete_stack` is one transaction: when the stack-row delete fails the
/// instance deletes are rolled back with it.
async fn failed_delete_stack_leaves_instances(store: Arc<dyn Store>, pool: &sqlx::SqlitePool) {
    store
        .commit_stack_plan(&StackPlan::replicas(
            "shop",
            "{}",
            "yaml",
            vec![("web", vec![spec("alpine"), spec("alpine")])],
        ))
        .await
        .unwrap();

    sqlx::query(
        "CREATE TRIGGER no_down BEFORE DELETE ON stacks BEGIN SELECT RAISE(ABORT, 'no'); END",
    )
    .execute(pool)
    .await
    .unwrap();

    assert!(store.delete_stack("shop").await.is_err());
    assert!(store.get_stack("shop").await.unwrap().is_some());
    assert_eq!(
        store.list_instances().await.unwrap().len(),
        2,
        "instance deletes rolled back"
    );
}

#[tokio::test]
async fn creates_updates_and_prunes_memory() {
    creates_updates_and_prunes(memory().await).await;
}

#[tokio::test]
async fn creates_updates_and_prunes_sqlite() {
    let (store, _dir) = sqlite().await;
    creates_updates_and_prunes(store).await;
}

#[tokio::test]
async fn update_preserves_placement_and_observed_state_memory() {
    update_preserves_placement_and_observed_state(memory().await).await;
}

#[tokio::test]
async fn update_preserves_placement_and_observed_state_sqlite() {
    let (store, _dir) = sqlite().await;
    update_preserves_placement_and_observed_state(store).await;
}

#[tokio::test]
async fn failed_commit_rolls_back_sqlite() {
    let (store, _dir) = sqlite_concrete().await;
    failed_commit_rolls_back(store.clone(), store.pool()).await;
}

#[tokio::test]
async fn failed_delete_stack_leaves_instances_sqlite() {
    let (store, _dir) = sqlite_concrete().await;
    failed_delete_stack_leaves_instances(store.clone(), store.pool()).await;
}
