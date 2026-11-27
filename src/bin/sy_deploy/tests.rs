//! The orders and the runner end to end, against an SY.admin and a world in memory
//! (`world::fake`): a birth's ten steps, the request key, the checks, the tenant scope, a failure
//! and its resume, a restart in the middle, the removal and its refusals, the catalog and the tree.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::service::{Deployer, Job, Timings};
use crate::source::tests::plain_blanks;
use crate::source::NATIVE_SOURCES;
use crate::store::tests::temp_store;
use crate::store::{DeploymentState, Store};
use crate::world::fake::{Fake, OTHER_TENANT, TENANT};
use crate::world::{AdminApi, World};

fn timings() -> Timings {
    Timings {
        retry_backoff: vec![Duration::ZERO; 3],
        status_budget: Duration::from_secs(2),
        status_call: Duration::from_secs(1),
        check_call: Duration::from_secs(1),
    }
}

fn open(fake: &Arc<Fake>, store: &Store) -> (Arc<Deployer>, mpsc::UnboundedReceiver<Job>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let admin: Arc<dyn AdminApi> = fake.clone();
    let world: Arc<dyn World> = fake.clone();
    let deployer = Deployer::open(admin, world, store.clone(), timings(), tx).expect("opened");
    (deployer, rx)
}

async fn drain(deployer: &Deployer, rx: &mut mpsc::UnboundedReceiver<Job>) {
    while let Ok(job) = rx.try_recv() {
        deployer.run_job(job).await;
    }
}

fn order(key: &str) -> Value {
    json!({
        "tenant_id": TENANT,
        "request_key": key,
        "source": {"id": "plain-ed", "version": "1"},
        "grouping": {"kind": "ed"},
        "blanks": plain_blanks(),
        "_caller": {"by": "cloud", "tenant_id": TENANT},
    })
}

fn code(answer: &Value) -> &str {
    answer["error_code"].as_str().unwrap_or("")
}

const BIRTH_WRITES: [&str; 8] = [
    "create_virtual_person",
    "set_person_document",
    "run_node AI.",
    "set_ilk_works_for",
    "set_ilk_definition",
    "run_node IO.wapp.",
    "set_ilk_works_for",
    "vault_assign acme-laura-wapp",
];

fn assert_writes(fake: &Fake, expected: &[&str]) {
    let writes = fake.writes();
    assert_eq!(writes.len(), expected.len(), "{writes:#?}");
    for (write, expected) in writes.iter().zip(expected) {
        assert!(
            write.starts_with(expected),
            "{write} is not {expected}: {writes:#?}"
        );
    }
}

#[tokio::test]
async fn a_birth_answers_accepted_runs_its_ten_steps_and_ends_active() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-1"))
        .await;
    assert_eq!(answer["status"], json!("ok"), "{answer}");
    assert_eq!(answer["accepted"], json!(true));
    assert_eq!(answer["state"], json!("deploying"));
    assert_eq!(answer["source"]["version"], json!("1"));
    let deployment_id = answer["deployment_id"].as_str().expect("an id").to_string();
    assert!(crate::ids::is_deployment_id(&deployment_id));
    let short_id = answer["grouping"]["short_id"]
        .as_str()
        .expect("a short id")
        .to_string();
    assert!(crate::ids::is_short_id(&short_id, "ed"));
    let person = answer["grouping"]["id"]
        .as_str()
        .expect("the ED")
        .to_string();
    // Recorded and journaled, with its ids, before any step; the checks wrote nothing.
    let record = deployer.record(&deployment_id).expect("recorded");
    assert_eq!(record.state, DeploymentState::Deploying);
    assert_eq!(record.nodes[0].node_name, format!("AI.{short_id}@worker1"));
    assert_eq!(
        record.nodes[1].node_name,
        format!("IO.wapp.{short_id}@worker1")
    );
    assert_eq!(record.nodes[0].runtime_version, "0.1.159");
    assert_eq!(record.assets["conscious"].handbooks.len(), 1);
    assert_eq!(record.vault[0].assigned_to, record.nodes[1].node_name);
    let journal = store.read_journal(&deployment_id);
    assert_eq!(journal[0]["event"], json!("accepted"));
    assert_eq!(
        journal[0]["nodes"][1]["ilk_id"],
        json!(record.nodes[1].ilk_id)
    );
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());

    drain(&deployer, &mut rx).await;
    let record = deployer.record(&deployment_id).expect("recorded");
    assert_eq!(record.state, DeploymentState::Active, "{record:#?}");
    assert_writes(&fake, &BIRTH_WRITES);
    {
        let state = fake.state.lock().expect("fake");
        assert_eq!(
            state.assets.len(),
            3,
            "the role, the handbook and the personality"
        );
        let ed = &state.ilks[&person];
        assert!(ed.is_virtual && ed.tenant_id == TENANT);
        let conscious = &state.ilks[&record.nodes[0].ilk_id];
        assert_eq!(conscious.works_for.as_deref(), Some(person.as_str()));
        assert_eq!(conscious.work_role.as_deref(), Some("conscious"));
        assert_eq!(conscious.role_hash, record.assets["conscious"].role);
        let channel = &state.ilks[&record.nodes[1].ilk_id];
        assert_eq!(channel.work_role.as_deref(), Some("channel"));
        assert_eq!(
            state.secrets["acme-laura-wapp"].3,
            vec![record.nodes[1].ilk_id.clone()]
        );
        let io = &state.nodes[&record.nodes[1].node_name];
        assert_eq!(io.config["io"]["person_ilk"], json!(person));
        assert_eq!(
            (io.hive.as_str(), io.tenant_id.as_str()),
            ("worker1", TENANT)
        );
    }
    // Every step journaled before and after, the record's last.
    let journal = store.read_journal(&deployment_id);
    let done: Vec<&str> = journal
        .iter()
        .filter(|entry| entry["event"] == "step" && entry["phase"] == "done")
        .filter_map(|entry| entry["step"].as_str())
        .collect();
    assert_eq!(done.len(), 10, "{done:?}");
    assert_eq!(done.last(), Some(&"record_active"));
    let starts = journal
        .iter()
        .filter(|entry| entry["event"] == "step" && entry["phase"] == "start")
        .count();
    assert_eq!(starts, 10);

    // cd_get shows it, with each node's status and the steps' times.
    let got = deployer
        .handle("query", "cd_get", json!({"deployment_id": deployment_id, "_caller": {"by": "cloud", "tenant_id": TENANT}}))
        .await;
    assert_eq!(got["deployment"]["state"], json!("active"), "{got}");
    assert_eq!(
        got["deployment"]["nodes"][0]["status"]["lifecycle_state"],
        json!("RUNNING")
    );
    assert_eq!(
        got["deployment"]["progress"].as_array().map(Vec::len),
        Some(10)
    );
    assert!(got["deployment"].get("request_hash").is_none());
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn the_same_request_key_gives_the_same_deployment_and_other_content_is_refused() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let first = deployer
        .handle("command", "cd_deploy", order("cloud-ed-2"))
        .await;
    let again = deployer
        .handle("command", "cd_deploy", order("cloud-ed-2"))
        .await;
    assert_eq!(again["deployment_id"], first["deployment_id"], "{again}");
    assert_eq!(again["accepted"], json!(true));
    // A field sent as null is one left out.
    let mut nulls = order("cloud-ed-2");
    nulls["hive"] = Value::Null;
    nulls["grouping"]["id"] = Value::Null;
    nulls["blanks"]["personality"]["region_code"] = Value::Null;
    let same = deployer.handle("command", "cd_deploy", nulls).await;
    assert_eq!(same["deployment_id"], first["deployment_id"], "{same}");
    drain(&deployer, &mut rx).await;
    // Once active, the same order reads it back: no new run.
    fake.clear_calls();
    let active = deployer
        .handle("command", "cd_deploy", order("cloud-ed-2"))
        .await;
    assert_eq!(active["deployment_id"], first["deployment_id"]);
    assert_eq!(active["state"], json!("active"));
    assert_eq!(active["accepted"], json!(false));
    assert!(rx.try_recv().is_err());
    assert!(fake.writes().is_empty());
    let mut other = order("cloud-ed-2");
    other["blanks"]["display_name"] = json!("Ana");
    let reused = deployer.handle("command", "cd_deploy", other).await;
    assert_eq!(code(&reused), "REQUEST_KEY_REUSED", "{reused}");
    assert_eq!(lock_records(&deployer), 1);
    let _ = std::fs::remove_dir_all(store.root());
}

fn lock_records(deployer: &Deployer) -> usize {
    deployer.inner.lock().expect("inner").records.len()
}

#[tokio::test]
async fn the_checks_come_before_any_step_and_record_nothing() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, _rx) = open(&fake, &store);
    let refused = |change: &dyn Fn(&mut Value)| {
        let mut order = order("cloud-ed-check");
        change(&mut order);
        order
    };
    let cases: Vec<(Value, &str)> = vec![
        (
            refused(&|o| o["source"]["version"] = json!("9")),
            "SOURCE_NOT_FOUND",
        ),
        (
            refused(&|o| o["source"]["id"] = json!("nope")),
            "SOURCE_NOT_FOUND",
        ),
        (
            refused(&|o| o["blanks"]["whatsapp"]["phone_number_id"] = json!("12ab")),
            "BLANK_INVALID",
        ),
        (
            refused(&|o| o["blanks"]["extra"] = json!(1)),
            "BLANK_INVALID",
        ),
        (
            refused(&|o| o["grouping"]["kind"] = json!("infra")),
            "INVALID_REQUEST",
        ),
        (
            refused(&|o| o["grouping"]["id"] = json!("ilk:550e8400-e29b-41d4-a716-446655440001")),
            "NOT_FOUND",
        ),
        (
            refused(&|o| o["hive"] = json!("worker9")),
            "HIVE_UNAVAILABLE",
        ),
        (refused(&|o| o["color"] = json!("red")), "INVALID_REQUEST"),
        (
            refused(&|o| o["request_key"] = json!("has space")),
            "INVALID_REQUEST",
        ),
        (
            refused(&|o| o["tenant_id"] = json!(fluxbee_sdk::identity::DEFAULT_ROOT_TENANT_ID)),
            "NOT_FOUND",
        ),
        (
            refused(&|o| o["blanks"]["whatsapp"]["credential_key"] = json!("someone-else")),
            "CREDENTIAL_MISSING",
        ),
    ];
    for (order, expected) in cases {
        let answer = deployer.handle("command", "cd_deploy", order.clone()).await;
        assert_eq!(code(&answer), expected, "{order} -> {answer}");
    }
    // The operator (no claimed tenant) on the root tenant, and a tenant not active.
    let mut root = order("cloud-ed-root");
    root["tenant_id"] = json!(fluxbee_sdk::identity::DEFAULT_ROOT_TENANT_ID);
    root["_caller"] = json!({"by": "operator", "tenant_id": null});
    assert_eq!(
        code(&deployer.handle("command", "cd_deploy", root).await),
        "INVALID_REQUEST"
    );
    fake.state
        .lock()
        .expect("fake")
        .tenants
        .insert(TENANT.to_string(), "suspended".to_string());
    assert_eq!(
        code(&deployer.handle("command", "cd_deploy", order("k")).await),
        "INVALID_REQUEST"
    );
    fake.state
        .lock()
        .expect("fake")
        .tenants
        .insert(TENANT.to_string(), "active".to_string());
    // No AI key of the agent's provider, then no live worker.
    fake.state
        .lock()
        .expect("fake")
        .secrets
        .remove("pruebas-anthropic");
    let answer = deployer.handle("command", "cd_deploy", order("k")).await;
    assert_eq!(code(&answer), "AI_KEY_MISSING", "{answer}");
    fake.state.lock().expect("fake").workers[0].alive = false;
    let answer = deployer.handle("command", "cd_deploy", order("k")).await;
    assert_eq!(code(&answer), "HIVE_UNAVAILABLE");
    assert_eq!(answer["retryable"], json!(true));
    fake.state.lock().expect("fake").workers[0] = crate::world::HiveRoom {
        hive_id: "worker1".to_string(),
        alive: true,
        node_count: 1023,
    };
    assert_eq!(
        code(&deployer.handle("command", "cd_deploy", order("k")).await),
        "HIVE_UNAVAILABLE"
    );
    assert_eq!(lock_records(&deployer), 0);
    assert!(fake.writes().is_empty());
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn a_number_is_held_by_one_deployment() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, _rx) = open(&fake, &store);
    let first = deployer
        .handle("command", "cd_deploy", order("cloud-ed-a"))
        .await;
    assert_eq!(first["status"], json!("ok"));
    let second = deployer
        .handle("command", "cd_deploy", order("cloud-ed-b"))
        .await;
    assert_eq!(code(&second), "NUMBER_TAKEN", "{second}");
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn another_tenant_s_deployments_read_as_not_found() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-3"))
        .await;
    drain(&deployer, &mut rx).await;
    let id = answer["deployment_id"].clone();
    let other = json!({"by": "cloud", "tenant_id": OTHER_TENANT});
    let got = deployer
        .handle(
            "query",
            "cd_get",
            json!({"deployment_id": id, "_caller": other}),
        )
        .await;
    assert_eq!(code(&got), "NOT_FOUND", "{got}");
    let listed = deployer
        .handle("query", "cd_list", json!({"_caller": other}))
        .await;
    assert_eq!(listed["deployments"], json!([]), "{listed}");
    let listed = deployer
        .handle(
            "query",
            "cd_list",
            json!({"tenant_id": TENANT, "_caller": other}),
        )
        .await;
    assert_eq!(listed["deployments"], json!([]));
    let tree = deployer
        .handle("query", "cd_tree", json!({"_caller": other}))
        .await;
    assert_eq!(tree["eds"], json!([]), "{tree}");
    let removed = deployer
        .handle(
            "command",
            "cd_remove",
            json!({"deployment_id": id, "_caller": other}),
        )
        .await;
    assert_eq!(code(&removed), "NOT_FOUND");
    let mut deploy = order("cloud-ed-4");
    deploy["_caller"] = other.clone();
    assert_eq!(
        code(&deployer.handle("command", "cd_deploy", deploy).await),
        "NOT_FOUND"
    );
    // The operator reads every tenant's.
    let operator = json!({"by": "operator", "tenant_id": null});
    let listed = deployer
        .handle("query", "cd_list", json!({"_caller": operator}))
        .await;
    assert_eq!(listed["count"], json!(1), "{listed}");
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn a_failed_step_leaves_failed_and_the_same_order_resumes_with_only_what_is_missing() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    fake.state.lock().expect("fake").refused.insert(
        "set_ilk_definition".to_string(),
        "INVALID_DEFINITION".to_string(),
    );
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-5"))
        .await;
    let id = answer["deployment_id"].as_str().expect("id").to_string();
    drain(&deployer, &mut rx).await;
    let record = deployer.record(&id).expect("record");
    assert_eq!(record.state, DeploymentState::Failed);
    let failure = record.failure.expect("its failure");
    assert_eq!(failure.step, "set_ilk_definition:conscious");
    assert_eq!(failure.error_code, "INVALID_DEFINITION");
    assert_eq!(failure.operation, "deploy");

    fake.state.lock().expect("fake").refused.clear();
    fake.clear_calls();
    let resumed = deployer
        .handle("command", "cd_deploy", order("cloud-ed-5"))
        .await;
    assert_eq!(resumed["deployment_id"], json!(id));
    assert_eq!(resumed["accepted"], json!(true), "{resumed}");
    drain(&deployer, &mut rx).await;
    assert_eq!(
        deployer.record(&id).expect("record").state,
        DeploymentState::Active
    );
    assert_writes(
        &fake,
        &[
            "set_ilk_definition",
            "run_node IO.wapp.",
            "set_ilk_works_for",
            "vault_assign",
        ],
    );
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn a_step_with_no_answer_is_tried_again_after_reading_the_state() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    fake.state
        .lock()
        .expect("fake")
        .unanswered
        .insert("run_node".to_string(), 1);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-6"))
        .await;
    drain(&deployer, &mut rx).await;
    let id = answer["deployment_id"].as_str().expect("id");
    assert_eq!(
        deployer.record(id).expect("record").state,
        DeploymentState::Active
    );
    // The person was not created twice: the second try read it in the SHM.
    let creates = fake
        .writes()
        .iter()
        .filter(|w| w.starts_with("create_virtual_person"))
        .count();
    assert_eq!(creates, 1);
    // A step that never answers fails the deployment after its tries.
    let fake = Arc::new(Fake::installation());
    let store2 = temp_store();
    let (deployer, mut rx) = open(&fake, &store2);
    fake.state
        .lock()
        .expect("fake")
        .unanswered
        .insert("vault_assign".to_string(), 100);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-7"))
        .await;
    drain(&deployer, &mut rx).await;
    let record = deployer
        .record(answer["deployment_id"].as_str().expect("id"))
        .expect("record");
    assert_eq!(record.state, DeploymentState::Failed);
    assert_eq!(record.failure.expect("failure").error_code, "NO_ANSWER");
    let _ = std::fs::remove_dir_all(store.root());
    let _ = std::fs::remove_dir_all(store2.root());
}

#[tokio::test]
async fn a_restart_in_the_middle_resumes_and_converges_on_one_ed() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-8"))
        .await;
    let id = answer["deployment_id"].as_str().expect("id").to_string();
    let record = deployer.record(&id).expect("record");
    // The first SY.deploy made the person, its document and the conscious, then stopped.
    let job = rx.try_recv().expect("its job");
    assert_eq!(job, Job::Deploy(id.clone()));
    let desired = deployer.desired_for(&record).expect("desired");
    let steps = crate::steps::build_steps(&desired, &crate::steps::Observed::default());
    for step in steps.iter().take(4) {
        if let Some(call) = &step.call {
            fake.call(call).await;
        }
        if let crate::steps::StepKind::WriteAssets { .. } = step.kind {
            for asset in desired.nodes[0].definition.as_ref().expect("def").assets() {
                fake.write_asset(&asset.value).expect("asset");
            }
        }
    }
    drop(deployer);
    drop(rx);
    fake.clear_calls();
    let (deployer, mut rx) = open(&fake, &store);
    assert_eq!(
        rx.try_recv().ok(),
        Some(Job::Deploy(id.clone())),
        "a deploying record resumes"
    );
    deployer.run_job(Job::Deploy(id.clone())).await;
    assert_eq!(
        deployer.record(&id).expect("record").state,
        DeploymentState::Active
    );
    assert_writes(
        &fake,
        &[
            "set_ilk_works_for",
            "set_ilk_definition",
            "run_node IO.wapp.",
            "set_ilk_works_for",
            "vault_assign",
        ],
    );
    // One ED, one ILK per node.
    let state = fake.state.lock().expect("fake");
    assert_eq!(state.ilks.values().filter(|ilk| ilk.is_virtual).count(), 1);
    assert_eq!(state.nodes.len(), 2);
    drop(state);
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn a_grouping_s_removal_retires_the_io_first_then_the_conscious_then_marks_the_ed() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-9"))
        .await;
    // A removal while the deployment runs is refused, nothing changed.
    let person = answer["grouping"]["id"].as_str().expect("ed").to_string();
    let busy = deployer
        .handle(
            "command",
            "cd_remove",
            json!({"grouping_id": person, "_caller": {"by": "cloud", "tenant_id": TENANT}}),
        )
        .await;
    assert_eq!(code(&busy), "BUSY", "{busy}");
    drain(&deployer, &mut rx).await;
    let id = answer["deployment_id"].as_str().expect("id").to_string();
    let record = deployer.record(&id).expect("record");
    fake.clear_calls();
    let removing = deployer
        .handle(
            "command",
            "cd_remove",
            json!({"grouping_id": person, "_caller": {"by": "cloud", "tenant_id": TENANT}}),
        )
        .await;
    assert_eq!(removing["accepted"], json!(true), "{removing}");
    assert_eq!(removing["state"], json!("removing"));
    assert_eq!(
        deployer.record(&id).expect("record").state,
        DeploymentState::Removing
    );
    drain(&deployer, &mut rx).await;
    assert_eq!(
        deployer.record(&id).expect("record").state,
        DeploymentState::Removed
    );
    let calls: Vec<(String, Value)> = fake
        .calls()
        .into_iter()
        .map(|call| (call.action, call.params))
        .collect();
    assert_eq!(calls.len(), 3, "{calls:#?}");
    assert_eq!(calls[0].0, "kill_node");
    assert_eq!(
        calls[0].1,
        json!({"node_name": record.nodes[1].node_name, "retire_instance": true})
    );
    assert_eq!(calls[1].1["node_name"], json!(record.nodes[0].node_name));
    assert_eq!(
        calls[2],
        (
            "delete_ilk".to_string(),
            json!({"ilk_id": person, "tenant_id": TENANT})
        )
    );
    // Asked again: already removed.
    let again = deployer
        .handle("command", "cd_remove", json!({"deployment_id": id}))
        .await;
    assert_eq!(again["state"], json!("removed"), "{again}");
    assert_eq!(again["accepted"], json!(false));
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn a_retire_whose_mark_failed_is_marked_by_sy_deploy() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-10"))
        .await;
    drain(&deployer, &mut rx).await;
    let id = answer["deployment_id"].as_str().expect("id").to_string();
    let record = deployer.record(&id).expect("record");
    fake.state.lock().expect("fake").retire_marks = false;
    fake.clear_calls();
    deployer
        .handle("command", "cd_remove", json!({"deployment_id": id}))
        .await;
    drain(&deployer, &mut rx).await;
    assert_eq!(
        deployer.record(&id).expect("record").state,
        DeploymentState::Removed
    );
    let writes = fake.writes();
    assert_eq!(
        writes,
        [
            format!("kill_node {}", record.nodes[1].node_name),
            format!("delete_ilk {}", record.nodes[1].ilk_id),
            format!("kill_node {}", record.nodes[0].node_name),
            format!("delete_ilk {}", record.nodes[0].ilk_id),
            format!("delete_ilk {}", record.grouping.id),
        ]
    );
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn the_founding_deployment_alone_is_not_removable_while_others_exist() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-11"))
        .await;
    drain(&deployer, &mut rx).await;
    let id = answer["deployment_id"].as_str().expect("id").to_string();
    // A later deployment in the grouping (a responsibility, in a later stage), recorded so.
    let mut later = deployer.record(&id).expect("record");
    later.deployment_id = "dp2laterrr".to_string();
    later.founding = false;
    later.request_key = "later".to_string();
    later.created_at_ms += 1000;
    for (n, node) in later.nodes.iter_mut().enumerate() {
        node.role = format!("{}x", node.role);
        node.node_name = format!("AI.collections{n}.{}@worker1", later.grouping.short_id);
        node.ilk_id = crate::ids::mint_ilk_id();
    }
    deployer
        .inner
        .lock()
        .expect("inner")
        .records
        .insert(later.deployment_id.clone(), later.clone());
    let refused = deployer
        .handle("command", "cd_remove", json!({"deployment_id": id}))
        .await;
    assert_eq!(code(&refused), "DEPLOYMENT_NOT_REMOVABLE", "{refused}");
    let both = deployer
        .handle(
            "command",
            "cd_remove",
            json!({"grouping_id": later.grouping.short_id}),
        )
        .await;
    assert_eq!(
        both["deployment_ids"],
        json!(["dp2laterrr", id]),
        "newest first: {both}"
    );
    let one = deployer
        .handle(
            "command",
            "cd_remove",
            json!({"deployment_id": id, "grouping_id": "x"}),
        )
        .await;
    assert_eq!(code(&one), "INVALID_REQUEST");
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn the_catalog_lists_the_plain_ed_with_its_blanks() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, _rx) = open(&fake, &store);
    let catalog = deployer.handle("query", "cd_catalog", json!({})).await;
    let source = &catalog["sources"][0];
    assert_eq!(source["id"], json!("plain-ed"), "{catalog}");
    assert_eq!(source["kind"], json!("ed"));
    assert_eq!(source["newest"], json!("1"));
    assert_eq!(source["founds"], json!(true));
    let blanks: Vec<&str> = source["versions"][0]["blanks"]
        .as_array()
        .expect("blanks")
        .iter()
        .filter_map(|blank| blank["name"].as_str())
        .collect();
    assert_eq!(
        blanks,
        ["display_name", "company_name", "personality", "whatsapp"]
    );
    assert_eq!(
        source["versions"][0]["blanks"][3]["resource_type"],
        json!("whatsapp")
    );
    // Its copy is kept.
    assert_eq!(store.source_copies().len(), 1);
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn three_versions_of_a_source_are_kept_and_every_one_a_deployment_runs() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let raw: Value = serde_json::from_str(NATIVE_SOURCES[0]).expect("json");
    for version in ["2", "3", "4", "5"] {
        let mut copy = raw.clone();
        copy["version"] = json!(version);
        let parsed = crate::source::parse_source(&copy.to_string()).expect("a version");
        store
            .keep_source_copy("plain-ed", version, &parsed.canonical)
            .expect("kept");
    }
    // A deployment of version 1 runs.
    let mut record = crate::desired::tests::plain_record();
    record.source.hash = crate::source::parse_source(NATIVE_SOURCES[0])
        .expect("v1")
        .hash;
    store.save_deployment(&record).expect("saved");
    let (deployer, _rx) = open(&fake, &store);
    let versions: Vec<String> = store
        .source_copies()
        .into_iter()
        .map(|(_, version, _)| version)
        .collect();
    assert_eq!(versions, ["1", "3", "4", "5"]);
    let catalog = deployer.handle("query", "cd_catalog", json!({})).await;
    assert_eq!(catalog["sources"][0]["newest"], json!("5"));
    // Once nothing runs version 1, it goes too.
    deployer.update_record(&record.deployment_id, |record| {
        record.state = DeploymentState::Removed
    });
    deployer.refresh_catalog().expect("refreshed");
    let versions: Vec<String> = store
        .source_copies()
        .into_iter()
        .map(|(_, version, _)| version)
        .collect();
    assert_eq!(versions, ["3", "4", "5"]);
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn the_tree_shows_the_ed_its_two_parts_and_its_deployment() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, mut rx) = open(&fake, &store);
    let answer = deployer
        .handle("command", "cd_deploy", order("cloud-ed-12"))
        .await;
    drain(&deployer, &mut rx).await;
    let tree = deployer
        .handle(
            "query",
            "cd_tree",
            json!({"tenant_id": TENANT, "_caller": {"by": "cloud", "tenant_id": TENANT}}),
        )
        .await;
    assert_eq!(tree["status"], json!("ok"), "{tree}");
    let ed = &tree["eds"][0];
    assert_eq!(ed["ilk_id"], answer["grouping"]["id"]);
    assert_eq!(ed["document_state"], json!("ok"));
    assert_eq!(ed["document"]["short_id"], answer["grouping"]["short_id"]);
    assert_eq!(ed["parts"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        ed["parts"][0]["status"]["lifecycle_state"],
        json!("RUNNING")
    );
    assert_eq!(ed["deployments"][0]["state"], json!("active"));
    assert_eq!(
        ed["deployments"][0]["credentials"][0]["key"],
        json!("acme-laura-wapp")
    );
    assert_eq!(
        code(
            &deployer
                .handle("query", "cd_tree", json!({"_caller": {"by": "operator"}}))
                .await
        ),
        "INVALID_REQUEST"
    );
    let _ = std::fs::remove_dir_all(store.root());
}

#[tokio::test]
async fn an_order_of_the_wrong_kind_or_unknown_is_refused() {
    let fake = Arc::new(Fake::installation());
    let store = temp_store();
    let (deployer, _rx) = open(&fake, &store);
    assert_eq!(
        code(&deployer.handle("query", "cd_deploy", order("k")).await),
        "INVALID_REQUEST"
    );
    assert_eq!(
        code(&deployer.handle("command", "cd_list", json!({})).await),
        "INVALID_REQUEST"
    );
    assert_eq!(
        code(&deployer.handle("command", "cd_publish", json!({})).await),
        "INVALID_REQUEST"
    );
    assert_eq!(
        code(
            &deployer
                .handle("query", "cd_get", json!({"deployment_id": "dp2nothere"}))
                .await
        ),
        "NOT_FOUND"
    );
    let _ = std::fs::remove_dir_all(store.root());
}
