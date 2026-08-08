//! Integration: SQLite store survives process reopen (same data dir).

use mc2_store::{hash_token, SqliteStore, Store};
use mc2_tests::TestCluster;
use tempfile::tempdir;

#[tokio::test]
async fn cluster_meta_survives_reopen() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("mc2.db");

    {
        let store = SqliteStore::open(&db).await.unwrap();
        assert!(store.get_cluster_meta().await.unwrap().is_none());
        store
            .init_cluster(&hash_token("api-persist"))
            .await
            .unwrap();
    }

    let store = SqliteStore::open(&db).await.unwrap();
    let meta = store.get_cluster_meta().await.unwrap().expect("meta");
    assert!(meta.initialized);
    assert!(store.verify_api_token("api-persist").await.unwrap());
    assert!(!store.verify_api_token("wrong").await.unwrap());
}

#[tokio::test]
async fn test_cluster_data_dir_has_db_and_secrets_key() {
    let cluster = TestCluster::start().await.expect("start");
    assert!(cluster.data_dir.join("mc2.db").is_file());
    assert!(cluster.data_dir.join("secrets.key").is_file());
    let key = std::fs::read(cluster.data_dir.join("secrets.key")).unwrap();
    assert!(key.len() >= 32);
}
