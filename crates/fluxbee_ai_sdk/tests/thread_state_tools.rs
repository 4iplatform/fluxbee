use std::path::PathBuf;
use std::sync::Arc;

use fluxbee_ai_sdk::{
    FunctionTool, FunctionToolProvider, LanceDbThreadStateStore, ThreadStateDeleteTool, ThreadStateGetTool,
    ThreadStatePutTool, ThreadStateStore, ThreadStateToolsProvider,
};
use serde_json::json;

fn unique_temp_store_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxbee_ai_sdk_thread_state_tools_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

#[tokio::test]
async fn thread_state_get_returns_found_false_when_record_is_missing() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");

    let tool = ThreadStateGetTool::new(Arc::new(store));
    let out = tool
        .call(json!({ "thread_id": "thr-missing" }))
        .await
        .expect("tool call should succeed");

    assert_eq!(out.get("found").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        out.get("thread_id").and_then(|v| v.as_str()),
        Some("thr-missing")
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn thread_state_get_returns_record_when_present() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");
    store
        .put("thr-1", json!({"status":"open","counter":2}), Some(3600))
        .await
        .expect("seed record");

    let tool = ThreadStateGetTool::new(Arc::new(store));
    let out = tool
        .call(json!({ "thread_id": "thr-1" }))
        .await
        .expect("tool call should succeed");

    assert_eq!(out.get("found").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("thread_id").and_then(|v| v.as_str()), Some("thr-1"));
    assert_eq!(
        out.get("data").and_then(|v| v.get("status")).and_then(|v| v.as_str()),
        Some("open")
    );
    assert_eq!(
        out.get("ttl_seconds").and_then(|v| v.as_u64()),
        Some(3600)
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn thread_state_put_writes_record_visible_to_store_get() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");

    let store_arc: Arc<dyn ThreadStateStore> = Arc::new(store.clone());
    let put_tool = ThreadStatePutTool::new(store_arc.clone());

    let out = put_tool
        .call(json!({
            "thread_id": "thr-put-1",
            "data": { "status": "waiting_information", "counter": 1 },
            "ttl_seconds": 120
        }))
        .await
        .expect("put tool call should succeed");

    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        out.get("thread_id").and_then(|v| v.as_str()),
        Some("thr-put-1")
    );

    let record = store
        .get("thr-put-1")
        .await
        .expect("store get")
        .expect("record should exist");
    assert_eq!(
        record
            .data
            .get("status")
            .and_then(|v| v.as_str()),
        Some("waiting_information")
    );
    assert_eq!(record.ttl_seconds, Some(120));

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn thread_state_put_overwrites_existing_record() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");
    store
        .put("thr-put-2", json!({"status":"old"}), Some(10))
        .await
        .expect("seed record");

    let put_tool = ThreadStatePutTool::new(Arc::new(store.clone()));
    put_tool
        .call(json!({
            "thread_id": "thr-put-2",
            "data": { "status": "new", "counter": 2 }
        }))
        .await
        .expect("put tool should overwrite");

    let record = store
        .get("thr-put-2")
        .await
        .expect("store get")
        .expect("record should exist");
    assert_eq!(record.data.get("status").and_then(|v| v.as_str()), Some("new"));
    assert_eq!(record.data.get("counter").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(record.ttl_seconds, None);

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn thread_state_delete_removes_existing_record() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");
    store
        .put("thr-del-1", json!({"status":"open"}), None)
        .await
        .expect("seed record");

    let delete_tool = ThreadStateDeleteTool::new(Arc::new(store.clone()));
    let out = delete_tool
        .call(json!({ "thread_id": "thr-del-1" }))
        .await
        .expect("delete should succeed");

    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("thread_id").and_then(|v| v.as_str()), Some("thr-del-1"));
    let after = store.get("thr-del-1").await.expect("store get after delete");
    assert!(after.is_none());

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn thread_state_delete_is_idempotent_for_missing_record() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");

    let delete_tool = ThreadStateDeleteTool::new(Arc::new(store));
    let out = delete_tool
        .call(json!({ "thread_id": "thr-del-missing" }))
        .await
        .expect("delete should succeed for missing records");

    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        out.get("thread_id").and_then(|v| v.as_str()),
        Some("thr-del-missing")
    );

    let _ = tokio::fs::remove_dir_all(root).await;
}

#[tokio::test]
async fn scoped_provider_overrides_model_supplied_thread_id() {
    let root = unique_temp_store_root();
    let store = LanceDbThreadStateStore::new(root.clone());
    store.ensure_ready().await.expect("store ready");
    let store_arc: Arc<dyn ThreadStateStore> = Arc::new(store.clone());

    let provider =
        ThreadStateToolsProvider::with_get_put_delete_scoped(store_arc, "sim-thread-1");
    let mut registry = fluxbee_ai_sdk::FunctionToolRegistry::new();
    provider
        .register_tools(&mut registry)
        .expect("register scoped tools");

    let put_tool = registry
        .get("thread_state_put")
        .expect("thread_state_put tool exists");
    put_tool
        .call(json!({
            "thread_id": "identity_thread",
            "data": {"email":"noe@gmail.com"}
        }))
        .await
        .expect("put call should succeed");

    let scoped = store
        .get("sim-thread-1")
        .await
        .expect("get scoped record");
    assert!(scoped.is_some());
    let leaked = store
        .get("identity_thread")
        .await
        .expect("get leaked record");
    assert!(leaked.is_none());

    let _ = tokio::fs::remove_dir_all(root).await;
}
