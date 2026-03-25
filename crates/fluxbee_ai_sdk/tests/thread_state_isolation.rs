use std::path::PathBuf;

use fluxbee_ai_sdk::{LanceDbThreadStateStore, ThreadStateStore};
use serde_json::json;

fn unique_state_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxbee_ai_sdk_thread_state_isolation_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

#[tokio::test]
async fn same_thread_id_is_isolated_across_node_stores() {
    let state_dir = unique_state_dir();
    let node_a_root = LanceDbThreadStateStore::path_for_node(&state_dir, "AI.frontdesk");
    let node_b_root = LanceDbThreadStateStore::path_for_node(&state_dir, "AI.support");
    assert_ne!(node_a_root, node_b_root);

    let store_a = LanceDbThreadStateStore::new(node_a_root.clone());
    let store_b = LanceDbThreadStateStore::new(node_b_root.clone());
    store_a.ensure_ready().await.expect("store a ready");
    store_b.ensure_ready().await.expect("store b ready");

    store_a
        .put("thread-123", json!({"owner":"node-a"}), None)
        .await
        .expect("put a");
    store_b
        .put("thread-123", json!({"owner":"node-b"}), None)
        .await
        .expect("put b");

    let a = store_a
        .get("thread-123")
        .await
        .expect("get a")
        .expect("record a");
    let b = store_b
        .get("thread-123")
        .await
        .expect("get b")
        .expect("record b");

    assert_eq!(a.data.get("owner").and_then(|v| v.as_str()), Some("node-a"));
    assert_eq!(b.data.get("owner").and_then(|v| v.as_str()), Some("node-b"));

    let _ = tokio::fs::remove_dir_all(state_dir).await;
}

