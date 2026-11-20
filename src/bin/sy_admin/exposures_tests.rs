//! The tests of io.web's exposures in SY.admin (docs/io-web-v2-stage-2-1.md §5.1, §5.6, §6): the
//! registry, the five actions only IO.cloud's relay asks for, the read of them all, and Cloud's
//! origins in the pushes (docs/io-web-v2-stage-2-2.md §2). A scripted mesh answers for the edge,
//! io.web and SY.vault; the time is paused, so a call that goes unanswered waits its 5 s at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use fluxbee_sdk::protocol::{
    Message, IO_WEB_ERROR_KEY_UNAVAILABLE, IO_WEB_ERROR_UNAUTHORIZED, MSG_EDGE_WEB_ROUTES,
    MSG_EDGE_WEB_ROUTES_RESPONSE, MSG_UNREACHABLE,
};
use fluxbee_sdk::RouterDispatcherTestHarness;

use super::*;
use crate::tests::{dst_of, mesh_test_admin, reply_to, test_temp_dir, write_registry_entry};
use crate::{
    admin_action_body_required, admin_action_body_required_fields, admin_action_is_read_only,
    admin_action_path_patterns, admin_action_timeout, authorize_mesh_caller,
    dispatch_internal_admin_command, enforce_cloud_relay_content, handle_send_node_message,
    mesh_access, resolve_internal_action_spec, CallerHive, MeshAccess, WebRoutes,
};

const RELAY: &str = "IO.cloud@motherbee";
const ARCHI: &str = "SY.architect@motherbee";
const IO_WEB: &str = "IO.web@motherbee";
const IO_WEB_ILK: &str = "ilk:3f6c2a1b-8d4e-4c5f-9a6b-7c8d9e0f1a2b";
const EDGE: &str = "SY.edge@ingress1";
const MAIN: &str = "hive-1.example.com";
const SHOP: &str = "shop.hive-1.example.com";
/// The test clock's start, 2026-11-18T00:00:00Z, in ms.
const START_MS: u64 = 1_794_960_000_000;
const START_SECS: u64 = START_MS / 1_000;

/// A registry in `state_dir` holding one closed exposure for each of `hosts`, each a label under
/// the main name MAIN, loaded as SY.admin's start loads it under MAIN.
pub(crate) fn registry_with_hosts(state_dir: &Path, hosts: &[&str]) -> Arc<Exposures> {
    let file = RegistryFile {
        schema_version: REGISTRY_SCHEMA_VERSION,
        generation: 1,
        exposures: hosts
            .iter()
            .enumerate()
            .map(|(index, host)| ExposureRecord {
                exposure_id: format!("{WEB_EXPOSURE_ID_PREFIX}{}", Uuid::new_v4()),
                label: host.split('.').next().unwrap_or_default().to_string(),
                host: host.to_string(),
                upstream: WebExposureUpstream::Address {
                    address: IpAddr::from([10, 0, 0, 30]),
                    port: 8_000 + index as u16,
                },
                published_at: 0,
                published_by: PUBLISHED_BY_CLOUD.to_string(),
                enablements: Vec::new(),
            })
            .collect(),
        cloud_origins: Vec::new(),
    };
    write_registry(&registry_path(state_dir), &file).expect("the registry");
    Exposures::load(
        state_dir,
        Some(MAIN),
        Arc::new(json_router::shm::now_epoch_ms),
    )
}

// ---------------------------------------------------------------------------------------------
// The scripted mesh
// ---------------------------------------------------------------------------------------------

/// How io.web answers a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoWebScript {
    /// It takes the set and holds every key.
    Ready,
    /// It takes the set and holds no key.
    KeysUnavailable,
    /// It does not answer.
    Silent,
    /// The router reaches no io.web.
    Absent,
}

/// How SY.vault answers a put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaultScript {
    /// It writes the key: its next version.
    Writes,
    /// It does not answer.
    Silent,
    /// It refuses the put.
    Refuses,
}

/// How the edge, io.web and SY.vault answer, and what SY.admin sent them.
struct Mesh {
    edge_answers: bool,
    /// The edge runs a version before 2.2: it refuses a set with any field but `routes`, as its
    /// `deny_unknown_fields` does, and names no `frame_origins`.
    edge_before_2_2: bool,
    io_web: IoWebScript,
    vault: VaultScript,
    /// io.web's answers to the links asked, in order, the last one repeated; `None` is silence.
    links: Vec<Option<Value>>,
    links_asked: usize,
    /// The version SY.vault holds of each key.
    versions: HashMap<String, i64>,
    /// The registry's file.
    registry: PathBuf,
    /// The registry on disk when each IO_WEB_EXPOSURES left (`None`: no file to read).
    on_disk_at_push: Vec<Option<RegistryFile>>,
    /// What SY.admin sent.
    sent: Vec<Message>,
}

impl Mesh {
    fn answer_links(&mut self, links: Vec<Option<Value>>) {
        self.links = links;
        self.links_asked = 0;
    }
}

/// The edge's answer to the routes in `payload`: a 2.2 edge takes them and names the
/// `frame_origins` it took beside its routes; one before 2.2 refuses a field it does not know.
fn edge_answer(payload: &Value, before_2_2: bool) -> Value {
    let unknown = payload
        .as_object()
        .and_then(|fields| fields.keys().find(|field| *field != "routes"));
    match unknown {
        Some(field) if before_2_2 => json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": format!(
                "invalid EDGE_WEB_ROUTES payload: unknown field `{field}`, expected `routes`"
            ),
        }),
        _ => {
            let mut taken = json!({ "status": "ok", "routes": payload["routes"] });
            if let Some(origins) = payload.get("frame_origins") {
                taken["frame_origins"] = origins.clone();
            }
            taken
        }
    }
}

/// io.web's answer to the set in `sent`.
fn io_web_answer(sent: &Message, script: IoWebScript) -> Option<Message> {
    let set: IoWebExposuresPayload =
        serde_json::from_value(sent.payload.clone()).expect("io.web's set");
    let key = match script {
        IoWebScript::Ready => IoWebKeyState::Ready,
        IoWebScript::KeysUnavailable => IoWebKeyState::Unavailable,
        IoWebScript::Silent => return None,
        IoWebScript::Absent => {
            return Some(reply_to(
                sent,
                MSG_UNREACHABLE,
                json!({ "reason": "NODE_NOT_FOUND", "original_dst": IO_WEB }),
            ))
        }
    };
    let keys = set
        .exposures
        .iter()
        .flat_map(|exposure| &exposure.enablements)
        .map(|enablement| IoWebEnablementKey {
            enablement_id: enablement.enablement_id.clone(),
            key,
            detail: (key == IoWebKeyState::Unavailable)
                .then(|| "the vault answered NOT_FOUND".to_string()),
        })
        .collect();
    let answer = IoWebExposuresResponse::Ok {
        generation: set.generation,
        changed: true,
        enablements: keys,
    };
    Some(reply_to(
        sent,
        MSG_IO_WEB_EXPOSURES_RESPONSE,
        serde_json::to_value(answer).expect("serializes"),
    ))
}

/// SY.vault's answer to the put in `sent`.
fn vault_answer(
    sent: &Message,
    script: VaultScript,
    versions: &mut HashMap<String, i64>,
) -> Option<Message> {
    let payload = match script {
        VaultScript::Writes => {
            let key = sent.payload["key"].as_str().expect("a key").to_string();
            let version = versions.entry(key).or_insert(0);
            *version += 1;
            json!({ "status": "ok", "version": *version })
        }
        VaultScript::Silent => return None,
        VaultScript::Refuses => json!({
            "status": "error",
            "error_code": "FORBIDDEN",
            "error_detail": "the caller may not write it",
        }),
    };
    Some(reply_to(sent, MSG_VAULT_PUT_RESPONSE, payload))
}

/// Answer what SY.admin sends, by `mesh`'s script, until nothing more comes for 300 ms.
async fn serve(harness: &mut RouterDispatcherTestHarness, mesh: &mut Mesh) {
    while let Some(sent) = harness
        .next_outgoing_within(Duration::from_millis(300))
        .await
    {
        let answer = match sent.meta.msg.as_deref() {
            Some(MSG_EDGE_WEB_ROUTES) if mesh.edge_answers => Some(reply_to(
                &sent,
                MSG_EDGE_WEB_ROUTES_RESPONSE,
                edge_answer(&sent.payload, mesh.edge_before_2_2),
            )),
            Some(MSG_IO_WEB_EXPOSURES) => {
                mesh.on_disk_at_push
                    .push(read_registry(&mesh.registry).ok().flatten());
                io_web_answer(&sent, mesh.io_web)
            }
            Some(MSG_IO_WEB_ACCESS_LINK) => {
                let link = mesh
                    .links
                    .get(mesh.links_asked)
                    .or(mesh.links.last())
                    .cloned()
                    .expect("a scripted link answer");
                mesh.links_asked += 1;
                link.map(|link| reply_to(&sent, MSG_IO_WEB_ACCESS_LINK_RESPONSE, link))
            }
            Some(MSG_VAULT_PUT) => vault_answer(&sent, mesh.vault, &mut mesh.versions),
            _ => None,
        };
        mesh.sent.push(sent);
        if let Some(answer) = answer {
            harness.inject(answer).await.expect("the answer");
        }
    }
}

/// An admin as Fluxbee Cloud finds it, the edge, io.web and SY.vault answering.
struct Admin {
    ctx: AdminContext,
    harness: RouterDispatcherTestHarness,
    mesh: Mesh,
    clock: Arc<AtomicU64>,
    edge: Option<&'static str>,
}

/// The main name hive-1.example.com; worker1 (10.0.0.21) and ingress1 (172.16.5.50) joined,
/// joining1 still joining; IO.web@motherbee launched (it has an ILK); `edge` the public edge (none
/// resolves without one); the clock at 2026-11-18T00:00:00Z.
fn exposures_admin(prefix: &str, edge: Option<&'static str>) -> Admin {
    let (mut ctx, harness) = mesh_test_admin(prefix);
    let registry = hive_registry(&ctx);
    let done = "join:\n  phase: done\n";
    for (hive, info) in [
        (
            "worker1",
            format!(
                "hive_id: worker1\nstatus: connected\nrole: worker\naddress: 10.0.0.21\n{done}"
            ),
        ),
        (
            "ingress1",
            format!(
                "hive_id: ingress1\nstatus: connected\nrole: ingress\naddress: 172.16.5.50\n{done}"
            ),
        ),
        (
            "joining1",
            "hive_id: joining1\nstatus: joining\nrole: worker\naddress: 10.0.0.23\n".to_string(),
        ),
    ] {
        write_registry_entry(registry.root(), hive, &info);
    }
    ctx.public_base_url = Some(format!("https://{MAIN}"));
    ctx.node_ilk =
        Arc::new(|_: &Path, node: &str| Ok((node == IO_WEB).then(|| IO_WEB_ILK.to_string())));
    let mesh = Mesh {
        edge_answers: true,
        edge_before_2_2: false,
        io_web: IoWebScript::Ready,
        vault: VaultScript::Writes,
        links: Vec::new(),
        links_asked: 0,
        versions: HashMap::new(),
        registry: registry_path(&ctx.state_dir),
        on_disk_at_push: Vec::new(),
        sent: Vec::new(),
    };
    let mut admin = Admin {
        ctx,
        harness,
        mesh,
        clock: Arc::new(AtomicU64::new(START_MS)),
        edge,
    };
    admin.restart();
    admin
}

impl Admin {
    /// What SY.admin's start does: the registry loaded from its file, on the test clock, under
    /// the main name `admin.public_base_url` gives, and the web routes over it.
    fn restart(&mut self) {
        let clock = Arc::clone(&self.clock);
        let main = acme::certified_name(self.ctx.public_base_url.as_deref()).ok();
        self.ctx.exposures = Exposures::load(
            &self.ctx.state_dir,
            main.as_deref(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        let edge = self.edge;
        self.ctx.web = Arc::new(WebRoutes::new(
            Arc::clone(&self.ctx.exposures),
            Arc::new(move || {
                edge.map(str::to_string)
                    .ok_or_else(|| "no connected ingress hive".to_string())
            }),
        ));
    }

    fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs * 1_000, Ordering::SeqCst);
    }

    /// IO.cloud's relay asks for `action`: the answer, and what SY.admin sent for it.
    async fn cloud(
        &mut self,
        action: &str,
        params: Value,
    ) -> (InternalAdminDispatchResult, Vec<Message>) {
        let Admin {
            ctx, harness, mesh, ..
        } = self;
        let (answer, ()) = tokio::join!(
            handle_exposure_write(ctx, &ctx.rpc, action, &params, AdminOrigin::Mesh(RELAY)),
            serve(harness, mesh)
        );
        (answer, std::mem::take(&mut mesh.sent))
    }

    /// A publish that succeeds: the exposure's id.
    async fn publish(&mut self, label: &str, upstream: Value) -> String {
        let (answer, _) = self
            .cloud(
                "publish_exposure",
                json!({ "label": label, "upstream": upstream }),
            )
            .await;
        assert_eq!(answer.http_status, 200, "{}", answer.envelope);
        answer.envelope["payload"]["exposure"]["exposure_id"]
            .as_str()
            .expect("an exposure id")
            .to_string()
    }

    /// An enable that succeeds: its payload.
    async fn enable(&mut self, exposure_id: &str, duration_s: u64) -> Value {
        let (answer, _) = self
            .cloud(
                "enable_exposure",
                json!({ "exposure_id": exposure_id, "duration_s": duration_s }),
            )
            .await;
        assert_eq!(answer.http_status, 200, "{}", answer.envelope);
        answer.envelope["payload"].clone()
    }

    /// What a run of publish_cloud_endpoint that names `origins` does once its door is open
    /// (`keep_cloud_origins`, docs/io-web-v2-stage-2-2.md §2): whether the file holds them, and
    /// what SY.admin sent.
    async fn keep_origins(&mut self, origins: &[&str]) -> (Result<(), String>, Vec<Message>) {
        let Admin {
            ctx, harness, mesh, ..
        } = self;
        let origins = origins.iter().map(|origin| origin.to_string()).collect();
        let (written, ()) = tokio::join!(
            crate::keep_cloud_origins(ctx, &ctx.rpc, origins),
            serve(harness, mesh)
        );
        (written, std::mem::take(&mut mesh.sent))
    }

    /// `GET /web/status`'s payload, io.web's own two reads left unanswered.
    async fn web_status(&mut self) -> Value {
        let Admin {
            ctx, harness, mesh, ..
        } = self;
        let limit = Duration::from_millis(50);
        let (payload, ()) = tokio::join!(
            crate::web_status_payload(ctx, &ctx.rpc, limit, limit),
            serve(harness, mesh)
        );
        mesh.sent.clear();
        payload
    }

    /// The registry's file as it is on disk.
    fn on_disk(&self) -> RegistryFile {
        read_registry(&self.mesh.registry)
            .expect("readable")
            .expect("written")
    }

    /// `list_exposures`'s views.
    fn views(&self) -> Vec<Value> {
        let answer = handle_list_exposures(&self.ctx, &Value::Null);
        assert_eq!(answer.http_status, 200, "{}", answer.envelope);
        answer.envelope["payload"]["exposures"]
            .as_array()
            .expect("the views")
            .clone()
    }
}

fn shop_upstream() -> Value {
    json!({ "kind": "hive", "hive": "worker1", "port": 8443 })
}

fn error_code(answer: &InternalAdminDispatchResult) -> &str {
    answer.envelope["error_code"].as_str().unwrap_or_default()
}

fn names_of(sent: &[Message]) -> Vec<&str> {
    let mut names: Vec<&str> = sent
        .iter()
        .map(|message| message.meta.msg.as_deref().unwrap_or_default())
        .collect();
    names.sort_unstable();
    names
}

fn sent_named<'a>(sent: &'a [Message], name: &str) -> &'a Message {
    sent.iter()
        .find(|message| message.meta.msg.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no {name} sent"))
}

fn set_of(message: &Message) -> IoWebExposuresPayload {
    serde_json::from_value(message.payload.clone()).expect("io.web's set")
}

/// The A-62 table for a caller on the motherbee or on a worker.
fn mesh_admits(caller: &str, action: &str) -> bool {
    let caller_hive = match caller.rsplit_once('@').map(|(_, hive)| hive) {
        Some("motherbee") => Some(CallerHive::Motherbee),
        Some(hive) if hive.starts_with("worker") => Some(CallerHive::Worker),
        _ => None,
    };
    authorize_mesh_caller(caller, caller_hive, action, None, &json!({}), "motherbee").is_ok()
}

fn link_ok(expires_at: u64) -> Option<Value> {
    Some(json!({
        "status": "ok",
        "url": format!("https://{SHOP}/.fluxbee/enter?code=c0de"),
        "expires_at": expires_at,
        "link_id": "0123456789abcdef",
    }))
}

fn link_refused(code: &str) -> Option<Value> {
    Some(json!({ "status": "error", "error_code": code, "error_detail": "scripted" }))
}

// ---------------------------------------------------------------------------------------------
// Who may ask (§5.1 (1)-(3))
// ---------------------------------------------------------------------------------------------

/// §5.1 (1), (2): the five actions are Fluxbee Cloud's alone. The mesh table gives them a class
/// of their own, which Archi does not reach; the handler refuses every origin but
/// IO.cloud@motherbee's relay, the HTTP API and the executor included, 403 UNAUTHORIZED before
/// anything is read or sent. IO.cloud's relay passes the handler whether or not the SDK's catalog
/// lists the action yet (docs/io-web-v2-stage-2-1.md §8: R4 adds them); the table admits it
/// exactly when the catalog does. The read, list_exposures, is Archi's too.
#[tokio::test(start_paused = true)]
async fn exposure_writes_are_iocloud_s_alone() {
    let mut admin = exposures_admin("web21-cloud-alone", Some(EDGE));
    for action in EXPOSURE_CLOUD_ACTIONS {
        assert_eq!(mesh_access(action), Some(MeshAccess::Cloud), "{action}");
        for caller in [
            ARCHI,
            "IO.api.x@motherbee",
            "AI.support@motherbee",
            "SY.wf-rules@worker1",
            "IO.cloud@worker1",
        ] {
            assert!(!mesh_admits(caller, action), "{caller} {action}");
        }
        assert_eq!(
            mesh_admits(RELAY, action),
            fluxbee_sdk::cloud::CLOUD_EXPOSED_ACTIONS.contains(action),
            "{action}"
        );
        for origin in [
            AdminOrigin::Http,
            AdminOrigin::Executor,
            AdminOrigin::Mesh(ARCHI),
            AdminOrigin::Mesh("IO.api.x@motherbee"),
            AdminOrigin::Mesh("IO.cloud@worker1"),
            AdminOrigin::Mesh(""),
        ] {
            let refused =
                handle_exposure_write(&admin.ctx, &admin.ctx.rpc, action, &json!({}), origin).await;
            assert_eq!(refused.http_status, 403, "{action} {origin:?}");
            assert_eq!(error_code(&refused), "UNAUTHORIZED");
            assert!(
                refused.envelope["error_detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("Fluxbee Cloud's alone")),
                "{}",
                refused.envelope
            );
        }
        // The relay passes: what answers is the action's own check of an empty body.
        let (answer, sent) = admin.cloud(action, json!({})).await;
        assert_eq!(answer.http_status, 400, "{action}: {}", answer.envelope);
        assert_eq!(error_code(&answer), "INVALID_REQUEST");
        assert!(sent.is_empty(), "{action}");
    }
    // Archi through the dispatcher: the table refuses it, nothing runs.
    let refused = dispatch_internal_admin_command(
        &admin.ctx,
        &admin.ctx.rpc,
        "publish_exposure",
        Some("motherbee"),
        json!({ "label": "shop", "upstream": { "kind": "archi" } }),
        AdminOrigin::Mesh(ARCHI),
    )
    .await
    .expect("answered");
    assert_eq!(refused.http_status, 403, "{}", refused.envelope);
    assert!(admin.ctx.exposures.file().exposures.is_empty());
    assert!(admin
        .harness
        .next_outgoing_within(Duration::from_millis(50))
        .await
        .is_none());
    // The read.
    assert_eq!(mesh_access(LIST_EXPOSURES), Some(MeshAccess::Archi));
    assert!(mesh_admits(ARCHI, LIST_EXPOSURES));
    assert!(!mesh_admits("AI.support@motherbee", LIST_EXPOSURES));
}

/// §5.1 (3): IO.cloud's relay carries each exposure action's own fields, and never a tenant: an
/// exposure is the installation's.
#[test]
fn iocloud_s_relay_carries_only_each_exposure_action_s_fields_and_no_tenant() {
    let id = format!("{WEB_EXPOSURE_ID_PREFIX}{}", Uuid::new_v4());
    let relayed = |action: &str, params: &Value| {
        enforce_cloud_relay_content(Some(RELAY), action, "motherbee", params)
    };
    for (action, params) in [
        (
            "publish_exposure",
            json!({ "label": "shop", "upstream": { "kind": "archi" } }),
        ),
        (
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 3600 }),
        ),
        ("disable_exposure", json!({ "exposure_id": id })),
        ("unpublish_exposure", json!({ "exposure_id": id })),
        ("issue_exposure_link", json!({ "exposure_id": id })),
        (LIST_EXPOSURES, json!({})),
    ] {
        assert_eq!(relayed(action, &params), Ok(()), "{action}");
        let mut with_tenant = params.clone();
        with_tenant["tenant_id"] = json!("tnt:5b1f2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d");
        let refused = relayed(action, &with_tenant).expect_err("no tenant");
        assert!(refused.contains("no tenant_id"), "{action}: {refused}");
        let mut with_another = params.clone();
        with_another["node_name"] = json!(IO_WEB);
        assert!(relayed(action, &with_another).is_err(), "{action}");
        // Another caller's body is the table's and the handler's to judge.
        assert_eq!(
            enforce_cloud_relay_content(Some(ARCHI), action, "motherbee", &with_tenant),
            Ok(())
        );
    }
    // Each action its own: the window is the enable's alone.
    for action in [
        "publish_exposure",
        "disable_exposure",
        "issue_exposure_link",
    ] {
        let params = json!({ "exposure_id": id, "duration_s": 60 });
        assert!(relayed(action, &params).is_err(), "{action}");
    }
    assert_eq!(relay_fields("vault_put"), None);
}

/// §5.1, §5.9: the six actions are in the registry with their help. The writes take a body of
/// exactly the fields the relay carries and have no HTTP route; the read is GET /web/exposures.
/// The longest write (a link asked twice around a push) answers within the time its caller waits.
#[test]
fn the_exposure_actions_are_registered_with_their_contracts() {
    for action in EXPOSURE_CLOUD_ACTIONS.iter().chain([&LIST_EXPOSURES]) {
        assert!(resolve_internal_action_spec(action).is_ok(), "{action}");
    }
    for action in EXPOSURE_CLOUD_ACTIONS {
        assert!(!admin_action_is_read_only(action), "{action}");
        assert!(admin_action_body_required(action), "{action}");
        assert!(
            admin_action_path_patterns(action).is_empty(),
            "{action}: Cloud's alone, no HTTP route"
        );
        let fields: Vec<String> = admin_action_body_required_fields(action)
            .iter()
            .map(|field| field["name"].as_str().expect("a name").to_string())
            .collect();
        assert_eq!(
            fields,
            relay_fields(action).expect("relayed").to_vec(),
            "{action}"
        );
        assert!(
            3 * EXPOSURE_CALL_TIMEOUT < admin_action_timeout(action),
            "{action}"
        );
    }
    assert!(admin_action_is_read_only(LIST_EXPOSURES));
    assert!(!admin_action_body_required(LIST_EXPOSURES));
    assert_eq!(
        admin_action_path_patterns(LIST_EXPOSURES),
        vec!["GET /web/exposures"]
    );
}

// ---------------------------------------------------------------------------------------------
// publish_exposure (§5.1)
// ---------------------------------------------------------------------------------------------

/// publish (1)-(6): the host is <label>.<main name>; the file holds the exposure before anything
/// is pushed; the edge's routes carry the host and io.web's set the exposure, which holds no
/// secret, both acknowledged; the answer is the exposure's view, closed. Without a main name
/// nothing is published.
#[tokio::test(start_paused = true)]
async fn publish_exposure_derives_the_host_under_the_main_name() {
    let mut admin = exposures_admin("web21-publish", Some(EDGE));
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["created"], true);
    let view = &answer.envelope["payload"]["exposure"];
    let id = view["exposure_id"].as_str().expect("an id");
    assert!(check_exposure_id(id).is_ok(), "{id}");
    assert_eq!(view["label"], "shop");
    assert_eq!(view["host"], SHOP);
    assert_eq!(view["url"], format!("https://{SHOP}/"));
    assert_eq!(view["upstream"], shop_upstream());
    assert_eq!(view["state"], "closed");
    assert_eq!(view["enabled_until"], Value::Null);
    assert_eq!(view["published_at"], "2026-11-18T00:00:00Z");
    assert_eq!(view["route"]["state"], "acknowledged");
    assert_eq!(view["io_web"]["state"], "acknowledged");
    assert_eq!(view["certificate"]["state"], "off", "no ACME in this test");

    assert_eq!(names_of(&sent), [MSG_EDGE_WEB_ROUTES, MSG_IO_WEB_EXPOSURES]);
    let routes = sent_named(&sent, MSG_EDGE_WEB_ROUTES);
    assert_eq!(dst_of(routes), json!(EDGE));
    assert_eq!(
        routes.payload,
        json!({ "routes": [{ "host": SHOP, "upstream": "io.web" }] })
    );
    let pushed = sent_named(&sent, MSG_IO_WEB_EXPOSURES);
    assert_eq!(dst_of(pushed), json!(IO_WEB));
    let set = set_of(pushed);
    assert_eq!(set, admin.ctx.exposures.io_web_payload());
    assert_eq!(set.exposures.len(), 1);
    assert_eq!(set.exposures[0].host, SHOP);
    assert!(set.exposures[0].enablements.is_empty());

    // The file held it when it was pushed, as memory does.
    let file = admin.on_disk();
    assert_eq!(admin.mesh.on_disk_at_push, vec![Some(file.clone())]);
    assert_eq!(file, admin.ctx.exposures.file());
    assert_eq!(file.exposures[0].published_by, "cloud");
    assert_eq!(file.exposures[0].published_at, START_SECS as i64);

    // An address upstream, IPv6 and private: its host too.
    let (answer, _) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "docs-2", "upstream": { "kind": "address", "address": "fd00::5", "port": 9443 } }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(
        answer.envelope["payload"]["exposure"]["host"],
        "docs-2.hive-1.example.com"
    );

    // No main name: nothing published, nothing sent.
    let mut admin = exposures_admin("web21-publish-no-main", Some(EDGE));
    admin.ctx.public_base_url = None;
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "NO_MAIN_HOST");
    assert!(sent.is_empty());
    assert!(!admin.mesh.registry.exists());
}

/// An edge and an io.web that do not answer: the exposure is published all the same (the file
/// holds it), its route and io.web pending for the loops to push again; with no edge, no_edge.
#[tokio::test(start_paused = true)]
async fn an_unanswered_publish_push_comes_back_pending() {
    let mut admin = exposures_admin("web21-publish-pending", Some(EDGE));
    admin.mesh.edge_answers = false;
    admin.mesh.io_web = IoWebScript::Silent;
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    let view = &answer.envelope["payload"]["exposure"];
    assert_eq!(view["route"]["state"], "pending");
    assert_eq!(view["io_web"]["state"], "pending");
    assert_eq!(names_of(&sent), [MSG_EDGE_WEB_ROUTES, MSG_IO_WEB_EXPOSURES]);
    assert_eq!(admin.on_disk().exposures.len(), 1);

    let mut admin = exposures_admin("web21-publish-no-edge", None);
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    let view = &answer.envelope["payload"]["exposure"];
    assert_eq!(view["route"]["state"], "no_edge");
    assert_eq!(view["io_web"]["state"], "acknowledged");
    assert_eq!(names_of(&sent), [MSG_IO_WEB_EXPOSURES]);

    // A stopped io.web (after kill_node IO.web@motherbee): the router reaches none, yet it has its
    // ILK: pending, not absent.
    admin.mesh.io_web = IoWebScript::Absent;
    let (answer, _) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "docs", "upstream": { "kind": "archi" } }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(
        answer.envelope["payload"]["exposure"]["io_web"]["state"],
        "pending"
    );
    assert_eq!(
        admin.ctx.exposures.io_web_status()["last_answer"]["state"],
        "absent"
    );
}

/// §2.1: a label is one DNS label, taken as given: refused, never folded, before anything is read
/// or written. Unknown fields are refused, never ignored.
#[tokio::test(start_paused = true)]
async fn publish_exposure_refuses_a_bad_label_and_never_folds_it() {
    let mut admin = exposures_admin("web21-labels", Some(EDGE));
    let long = "a".repeat(64);
    for label in [
        "Shop",
        "",
        "-shop",
        "shop-",
        "sh_op",
        "sh.op",
        "shop ",
        " shop",
        "shöp",
        long.as_str(),
    ] {
        let (answer, sent) = admin
            .cloud(
                "publish_exposure",
                json!({ "label": label, "upstream": shop_upstream() }),
            )
            .await;
        assert_eq!(answer.http_status, 400, "'{label}': {}", answer.envelope);
        assert_eq!(error_code(&answer), "INVALID_REQUEST");
        assert!(sent.is_empty(), "'{label}'");
    }
    let folded = check_label("Shop").expect_err("uppercase");
    assert!(folded.contains("never folded"), "{folded}");
    for params in [
        json!({ "label": "shop", "upstream": shop_upstream(), "tenant_id": "tnt:x" }),
        json!({ "label": "shop", "upstream": { "kind": "archi", "port": 3000 } }),
        json!({ "label": "shop", "upstream": { "kind": "ftp", "port": 21 } }),
        json!({ "label": "shop" }),
        json!({ "label": 7, "upstream": shop_upstream() }),
    ] {
        let (answer, sent) = admin.cloud("publish_exposure", params.clone()).await;
        assert_eq!(answer.http_status, 400, "{params}: {}", answer.envelope);
        assert!(sent.is_empty());
    }
    assert!(!admin.mesh.registry.exists(), "nothing written");
    // 63 characters, digits and inner hyphens: one label.
    let longest = format!("{}-9", "a".repeat(61));
    admin.publish(&longest, shop_upstream()).await;
    admin.publish("0-shop-1", shop_upstream()).await;
}

/// §2.1, §5.2: a label with hyphens as its 3rd and 4th characters is a reserved LDH label ('??--',
/// R-LDH), which Let's Encrypt refuses when the order is created: refused at publish, 400
/// INVALID_REQUEST naming the rule, nothing written or sent. Hyphens elsewhere are a label.
#[tokio::test(start_paused = true)]
async fn publish_exposure_refuses_a_reserved_ldh_label() {
    let mut admin = exposures_admin("web21-labels-rldh", Some(EDGE));
    for label in ["my--shop", "xn--zz"] {
        let (answer, sent) = admin
            .cloud(
                "publish_exposure",
                json!({ "label": label, "upstream": shop_upstream() }),
            )
            .await;
        assert_eq!(answer.http_status, 400, "'{label}': {}", answer.envelope);
        assert_eq!(error_code(&answer), "INVALID_REQUEST");
        let detail = answer.envelope["error_detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("3rd and 4th") && detail.contains("R-LDH"),
            "'{label}': {detail}"
        );
        assert!(sent.is_empty(), "'{label}'");
    }
    assert!(!admin.mesh.registry.exists(), "nothing written");
    admin.publish("a--shop", shop_upstream()).await;
    admin.publish("my-shop", shop_upstream()).await;
    admin.publish("myshop--1", shop_upstream()).await;
}

/// §2.2: an upstream is Archi, a worker of the registry, or a private address that is no hive's,
/// never one of Fluxbee's own ports.
#[tokio::test(start_paused = true)]
async fn publish_exposure_refuses_fluxbee_upstreams() {
    let mut admin = exposures_admin("web21-upstreams", Some(EDGE));
    let registry = hive_registry(&admin.ctx);
    let judged = |upstream: Value| {
        let upstream: WebExposureUpstream = serde_json::from_value(upstream).expect("parses");
        check_upstream(&upstream, "motherbee", &registry, &|_| false)
    };
    for (port, _) in REFUSED_UPSTREAM_PORTS {
        for upstream in [
            json!({ "kind": "hive", "hive": "worker1", "port": port }),
            json!({ "kind": "address", "address": "10.0.0.99", "port": port }),
        ] {
            let refused = judged(upstream).expect_err("a Fluxbee port");
            assert!(refused.contains("Fluxbee's own ports"), "{port}: {refused}");
        }
    }
    for (upstream, why) in [
        (
            json!({ "kind": "hive", "hive": "worker1", "port": 0 }),
            "no port",
        ),
        (
            json!({ "kind": "hive", "hive": "motherbee", "port": 8443 }),
            "is the motherbee",
        ),
        (
            json!({ "kind": "hive", "hive": "ingress1", "port": 8443 }),
            "is an ingress",
        ),
        (
            json!({ "kind": "hive", "hive": "joining1", "port": 8443 }),
            "not a connected worker",
        ),
        (
            json!({ "kind": "hive", "hive": "worker9", "port": 8443 }),
            "not a hive of the registry",
        ),
        (
            json!({ "kind": "hive", "hive": "../etc", "port": 8443 }),
            "not a hive id",
        ),
        (
            json!({ "kind": "address", "address": "10.0.0.21", "port": 8443 }),
            "the address of hive 'worker1'",
        ),
        (
            json!({ "kind": "address", "address": "::ffff:10.0.0.21", "port": 8443 }),
            "the address of hive 'worker1'",
        ),
        (
            json!({ "kind": "address", "address": "172.16.5.50", "port": 8443 }),
            "the address of hive 'ingress1'",
        ),
        (
            json!({ "kind": "address", "address": "127.0.0.1", "port": 8443 }),
            "loopback",
        ),
        (
            json!({ "kind": "address", "address": "::1", "port": 8443 }),
            "loopback",
        ),
        (
            json!({ "kind": "address", "address": "169.254.1.1", "port": 8443 }),
            "link-local",
        ),
        (
            json!({ "kind": "address", "address": "fe80::1", "port": 8443 }),
            "link-local",
        ),
        (
            json!({ "kind": "address", "address": "0.0.0.0", "port": 8443 }),
            "unspecified",
        ),
        (
            json!({ "kind": "address", "address": "224.0.0.1", "port": 8443 }),
            "multicast",
        ),
        (
            json!({ "kind": "address", "address": "255.255.255.255", "port": 8443 }),
            "broadcast",
        ),
        (
            json!({ "kind": "address", "address": "8.8.8.8", "port": 8443 }),
            "not private",
        ),
        (
            json!({ "kind": "address", "address": "2001:db8::1", "port": 8443 }),
            "not private",
        ),
    ] {
        let refused = judged(upstream.clone()).expect_err("refused");
        assert!(refused.contains(why), "{upstream}: {refused}");
    }
    for upstream in [
        json!({ "kind": "archi" }),
        json!({ "kind": "hive", "hive": "worker1", "port": 8443 }),
        json!({ "kind": "address", "address": "10.0.0.99", "port": 8443 }),
        json!({ "kind": "address", "address": "172.16.0.10", "port": 8000 }),
        json!({ "kind": "address", "address": "192.168.1.10", "port": 8000 }),
        json!({ "kind": "address", "address": "fd00::5", "port": 9443 }),
    ] {
        assert_eq!(judged(upstream.clone()), Ok(()), "{upstream}");
    }
    // A name is no address; the action answers 400 and writes nothing.
    for upstream in [
        json!({ "kind": "address", "address": "files.example.com", "port": 8443 }),
        json!({ "kind": "hive", "hive": "worker1", "port": 22 }),
    ] {
        let (answer, sent) = admin
            .cloud(
                "publish_exposure",
                json!({ "label": "shop", "upstream": upstream }),
            )
            .await;
        assert_eq!(answer.http_status, 400, "{}", answer.envelope);
        assert!(sent.is_empty());
    }
    assert!(!admin.mesh.registry.exists());
}

/// §2.2: the hive registry never holds the motherbee, so an address upstream that is one of the
/// motherbee's own (its LAN's, a second NIC's, docker0's) is judged by the host, which holds it
/// when a socket binds to it; and a registry that cannot be read refuses every address, since
/// any might be a hive's.
#[tokio::test(start_paused = true)]
async fn publish_exposure_refuses_the_motherbee_s_own_addresses_and_an_unreadable_registry() {
    let admin = exposures_admin("web21-own-addresses", Some(EDGE));
    let registry = hive_registry(&admin.ctx);
    let own: Vec<IpAddr> = ["192.168.1.7", "172.17.0.1", "fd00::7"]
        .iter()
        .map(|address| address.parse().expect("an address"))
        .collect();
    let holds = |address: &IpAddr| own.contains(address);
    let judged = |upstream: Value, registry: &HiveRegistry| {
        let upstream: WebExposureUpstream = serde_json::from_value(upstream).expect("parses");
        check_upstream(&upstream, "motherbee", registry, &holds)
    };
    for address in ["192.168.1.7", "::ffff:192.168.1.7", "172.17.0.1", "fd00::7"] {
        let upstream = json!({ "kind": "address", "address": address, "port": 8443 });
        let refused = judged(upstream, &registry).expect_err("the motherbee's own");
        assert!(
            refused.contains("the motherbee's own addresses"),
            "{address}: {refused}"
        );
    }
    let other = json!({ "kind": "address", "address": "192.168.1.8", "port": 8443 });
    assert_eq!(judged(other.clone(), &registry), Ok(()));
    // The host holds its loopback: the probe binds to it, and to no address it lacks.
    assert!(host_holds_address(&IpAddr::from([127, 0, 0, 1])));

    // No registry to read: refused, and saying why.
    let unreadable = HiveRegistry::new(test_temp_dir("web21-no-registry").join("hives"));
    let refused = judged(other, &unreadable).expect_err("an unreadable registry");
    assert!(
        refused.contains("cannot be read") && refused.contains("refused"),
        "{refused}"
    );
    let worker = json!({ "kind": "hive", "hive": "worker1", "port": 8443 });
    assert!(
        judged(worker, &unreadable).is_err(),
        "nor a worker it cannot read"
    );
}

/// The DNS a worker's recorded name resolves against in the tests: each name's answer, any other
/// unknown; `slow.example.com` never answers.
struct ScriptedDns(HashMap<&'static str, Result<Vec<IpAddr>, String>>);

#[async_trait::async_trait]
impl acme::NameResolver for ScriptedDns {
    async fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        if name == "slow.example.com" {
            std::future::pending::<()>().await;
        }
        self.0
            .get(name)
            .cloned()
            .unwrap_or_else(|| Err(format!("{name}: no such name")))
    }
}

/// FINDINGS A-358: io.web dials a worker at the address the registry records for it, a name
/// resolved, and refuses one outside the private ranges, loopback, link-local or the motherbee's
/// own (502, `address_refused`). SY.admin reads, resolves and checks it the same way at publish,
/// and refuses it `409 HIVE_ADDRESS_REFUSED`, naming the hive, the address and the rule; a private
/// one is published, and a name passes when one of its addresses does.
#[tokio::test(start_paused = true)]
async fn publish_exposure_refuses_a_worker_recorded_where_io_web_never_dials() {
    let mut admin = exposures_admin("web22-hive-address", Some(EDGE));
    let registry = hive_registry(&admin.ctx);
    let done = "join:\n  phase: done\n";
    for (hive, address) in [
        ("public1", Some("8.8.8.8")),
        ("loopback1", Some("127.0.0.1")),
        ("mapped1", Some("::ffff:127.0.0.1")),
        ("linklocal1", Some("169.254.10.10")),
        ("linklocal2", Some("fe80::10")),
        ("own1", Some("192.168.1.7")),
        ("ported1", Some("10.0.0.31:9000")),
        ("bracketed1", Some("[fd00::31]")),
        ("named1", Some("W1.Example.Internal")),
        ("mixed1", Some("w3.example.com")),
        ("namedpublic1", Some("w2.example.com")),
        ("empty1", Some("w4.example.com")),
        ("gone1", Some("gone.example.com")),
        ("slow1", Some("slow.example.com")),
        ("bad1", Some("not an address")),
        ("none1", None),
    ] {
        let address = address
            .map(|address| format!("address: \"{address}\"\n"))
            .unwrap_or_default();
        write_registry_entry(
            registry.root(),
            hive,
            &format!("hive_id: {hive}\nstatus: connected\nrole: worker\n{address}{done}"),
        );
    }
    let dns = ScriptedDns(HashMap::from([
        (
            "w1.example.internal",
            Ok(vec![IpAddr::from([10, 0, 0, 41])]),
        ),
        ("w2.example.com", Ok(vec![IpAddr::from([93, 184, 216, 34])])),
        (
            "w3.example.com",
            Ok(vec![
                IpAddr::from([127, 0, 0, 1]),
                IpAddr::from([10, 0, 0, 43]),
            ]),
        ),
        ("w4.example.com", Ok(Vec::new())),
    ]));
    let own: IpAddr = "192.168.1.7".parse().expect("an address");
    let holds = move |address: &IpAddr| *address == own;
    let judged = |hive: &'static str| check_hive_address(hive, &registry, &holds, &dns);

    for hive in ["worker1", "ported1", "bracketed1", "named1", "mixed1"] {
        assert_eq!(judged(hive).await, Ok(()), "{hive}");
    }
    for (hive, address, rule) in [
        ("public1", "8.8.8.8", "not private"),
        ("loopback1", "127.0.0.1", "loopback"),
        ("mapped1", "::ffff:127.0.0.1", "loopback"),
        ("linklocal1", "169.254.10.10", "link-local"),
        ("linklocal2", "fe80::10", "link-local"),
        ("own1", "192.168.1.7", "the motherbee's own addresses"),
        ("namedpublic1", "93.184.216.34", "not private"),
        ("empty1", "w4.example.com", "resolves to no address"),
        ("gone1", "gone.example.com", "does not resolve"),
        ("slow1", "slow.example.com", "did not resolve within"),
        (
            "bad1",
            "not an address",
            "no address an upstream can be dialled at",
        ),
        ("none1", "\"\"", "no address an upstream can be dialled at"),
    ] {
        let refused = judged(hive).await.expect_err("refused");
        assert!(
            refused.contains(&format!("upstream.hive '{hive}'"))
                && refused.contains(address)
                && refused.contains(rule),
            "{hive}: {refused}"
        );
    }
    let unreadable = HiveRegistry::new(test_temp_dir("web22-no-registry").join("hives"));
    let refused = check_hive_address("worker1", &unreadable, &holds, &dns)
        .await
        .expect_err("an unreadable registry");
    assert!(refused.contains("cannot be read"), "{refused}");

    // The action: 409, nothing written or pushed; the worker at a private address is published.
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({
                "label": "shop",
                "upstream": { "kind": "hive", "hive": "public1", "port": 8443 },
            }),
        )
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "HIVE_ADDRESS_REFUSED");
    let detail = answer.envelope["error_detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("public1") && detail.contains("8.8.8.8") && detail.contains("not private"),
        "{detail}"
    );
    assert!(sent.is_empty());
    assert!(!admin.mesh.registry.exists());
    admin.publish("shop", shop_upstream()).await;
}

/// §5.1 publish (2): the same label with the same upstream is the same exposure, answered again
/// with nothing written or pushed (`created: false`); another upstream under it is refused, and
/// one exposure leads to Archi.
#[tokio::test(start_paused = true)]
async fn publish_exposure_is_idempotent_for_the_same_upstream_and_refuses_another() {
    let mut admin = exposures_admin("web21-publish-again", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let before = admin.on_disk();
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["created"], false);
    assert_eq!(answer.envelope["payload"]["exposure"]["exposure_id"], id);
    assert!(sent.is_empty(), "nothing pushed again");
    assert_eq!(admin.on_disk(), before, "nothing written again");

    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": { "kind": "hive", "hive": "worker1", "port": 9443 } }),
        )
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_EXISTS");
    assert!(sent.is_empty());

    admin.publish("archi", json!({ "kind": "archi" })).await;
    let (answer, _) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "archi-2", "upstream": { "kind": "archi" } }),
        )
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_EXISTS");
    assert_eq!(admin.on_disk().exposures.len(), 2);
}

/// §2.1: at most 16 exposures; the same one asked again is still answered.
#[tokio::test(start_paused = true)]
async fn publish_exposure_stops_at_16() {
    let mut admin = exposures_admin("web21-limit", Some(EDGE));
    for index in 0..MAX_EXPOSURES {
        admin
            .publish(
                &format!("shop{index}"),
                json!({ "kind": "address", "address": "10.0.0.99", "port": 8000 + index }),
            )
            .await;
    }
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "one-more", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_LIMIT");
    assert!(sent.is_empty());
    assert_eq!(admin.on_disk().exposures.len(), MAX_EXPOSURES);
    let (answer, _) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop3", "upstream": { "kind": "address", "address": "10.0.0.99", "port": 8003 } }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["created"], false);
}

// ---------------------------------------------------------------------------------------------
// enable_exposure (§5.1, §3.1)
// ---------------------------------------------------------------------------------------------

/// enable (3)-(6): a new key, 64 hex, put first in the slot the current enablement does not use,
/// owned by io.web's ILK in the root tenant as a web_session_key; then the file, which holds its
/// slot and version and never the key; then io.web's set, with no secret. A renewal keeps the
/// enablement it renews; the one before that ends at once, its slot written again.
#[tokio::test(start_paused = true)]
async fn enable_puts_a_new_key_owned_by_io_web_in_the_free_slot_then_records_then_pushes() {
    let mut admin = exposures_admin("web21-enable", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let uuid = id.strip_prefix(WEB_EXPOSURE_ID_PREFIX).expect("exp:");

    let (answer, sent) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 3600 }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["renewed"], false);
    let view = &answer.envelope["payload"]["exposure"];
    assert_eq!(view["state"], "enabled");
    assert_eq!(view["enabled_until"], "2026-11-18T01:00:00Z");
    assert_eq!(view["io_web"]["state"], "acknowledged");
    let order: Vec<&str> = sent
        .iter()
        .map(|message| message.meta.msg.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(
        order,
        [MSG_VAULT_PUT, MSG_IO_WEB_EXPOSURES],
        "the put first"
    );
    let put = &sent[0];
    assert_eq!(dst_of(put), json!("SY.vault@motherbee"));
    assert_eq!(put.payload["key"], format!("web_exposure_key:{uuid}:a"));
    let secret = put.payload["value"]["secret"].as_str().expect("the key");
    assert_eq!(secret.len(), 64);
    assert!(secret
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    assert_eq!(put.payload["metadata"]["tenant_id"], DEFAULT_ROOT_TENANT_ID);
    assert_eq!(put.payload["metadata"]["resource_type"], "web_session_key");
    assert_eq!(put.payload["metadata"]["ilk"], IO_WEB_ILK);
    let raw = fs::read_to_string(&admin.mesh.registry).expect("the file");
    assert!(!raw.contains(secret), "the file never holds the key");
    assert!(!sent[1].payload.to_string().contains(secret), "nor the set");
    let first = admin.on_disk().exposures[0].enablements.clone();
    assert_eq!(first.len(), 1);
    // Opening persists before it opens (§2.3): the file held the enablement when the set left.
    let file_at_push = |admin: &Admin| {
        admin
            .mesh
            .on_disk_at_push
            .last()
            .cloned()
            .flatten()
            .expect("a file when IO_WEB_EXPOSURES left")
            .exposures[0]
            .enablements
            .clone()
    };
    assert_eq!(file_at_push(&admin), first, "written before the push");
    assert_eq!(
        (
            first[0].from,
            first[0].until,
            first[0].slot,
            first[0].key_version
        ),
        (START_SECS, START_SECS + 3_600, WebExposureKeySlot::A, 1)
    );
    assert_eq!(
        set_of(&sent[1]).exposures[0].enablements,
        vec![IoWebEnablement {
            enablement_id: first[0].enablement_id.clone(),
            until: START_SECS + 3_600,
            slot: WebExposureKeySlot::A,
            key_version: 1,
        }]
    );

    // A renewal: slot b; both live, the renewed one first.
    admin.advance(600);
    let (answer, sent) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 7200 }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["renewed"], true);
    assert_eq!(
        answer.envelope["payload"]["exposure"]["enabled_until"],
        "2026-11-18T02:10:00Z"
    );
    assert_eq!(sent[0].payload["key"], format!("web_exposure_key:{uuid}:b"));
    let second = admin.on_disk().exposures[0].enablements.clone();
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], first[0]);
    assert_eq!(file_at_push(&admin), second, "written before the push");
    assert_eq!(
        (second[1].slot, second[1].key_version),
        (WebExposureKeySlot::B, 1)
    );

    // Again: slot a, its version 2; the first enablement ends at once.
    admin.advance(600);
    let (answer, sent) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 60 }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(sent[0].payload["key"], format!("web_exposure_key:{uuid}:a"));
    let third = admin.on_disk().exposures[0].enablements.clone();
    assert_eq!(third.len(), 2);
    assert_eq!(third[0], second[1]);
    assert_eq!(
        (third[1].slot, third[1].key_version),
        (WebExposureKeySlot::A, 2)
    );
    assert_eq!(
        set_of(&sent[1]).exposures[0].enablements.len(),
        2,
        "io.web holds both live ones"
    );
    // The newest is the current one even when it ends first.
    assert_eq!(admin.views()[0]["enabled_until"], "2026-11-18T00:21:00Z");

    // io.web takes the set without the key: acknowledged all the same; the key's state is in
    // web_status.
    admin.mesh.io_web = IoWebScript::KeysUnavailable;
    let payload = admin.enable(&id, 3600).await;
    assert_eq!(payload["exposure"]["io_web"]["state"], "acknowledged");
    assert_eq!(
        admin.ctx.exposures.io_web_status()["last_answer"]["keys"][0]["key"],
        "unavailable"
    );
}

/// enable (3): a put that goes unanswered or is refused changes nothing: 502 VAULT_UNAVAILABLE,
/// nothing written, nothing pushed.
#[tokio::test(start_paused = true)]
async fn enable_answers_vault_unavailable_and_records_nothing_when_the_put_is_unanswered() {
    let mut admin = exposures_admin("web21-enable-vault", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let before = admin.on_disk();
    for (script, detail) in [
        (VaultScript::Silent, "did not answer"),
        (VaultScript::Refuses, "refused the put"),
    ] {
        admin.mesh.vault = script;
        let (answer, sent) = admin
            .cloud(
                "enable_exposure",
                json!({ "exposure_id": id, "duration_s": 3600 }),
            )
            .await;
        assert_eq!(answer.http_status, 502, "{}", answer.envelope);
        assert_eq!(error_code(&answer), "VAULT_UNAVAILABLE");
        assert!(
            answer.envelope["error_detail"]
                .as_str()
                .is_some_and(|text| text.contains(detail)),
            "{}",
            answer.envelope
        );
        assert_eq!(names_of(&sent), [MSG_VAULT_PUT], "{script:?}");
        assert_eq!(admin.on_disk(), before, "{script:?}");
        assert_eq!(admin.ctx.exposures.file(), before);
    }
}

/// enable (1): a window of one minute to 8 hours, a whole number of seconds; an exposure that is
/// not published is not found.
#[tokio::test(start_paused = true)]
async fn enable_refuses_more_than_8_hours() {
    let mut admin = exposures_admin("web21-enable-window", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    for duration in [
        json!(ENABLE_MAX_SECS + 1),
        json!(ENABLE_MIN_SECS - 1),
        json!(0),
        json!(-60),
        json!(90.5),
        json!("3600"),
        Value::Null,
    ] {
        let (answer, sent) = admin
            .cloud(
                "enable_exposure",
                json!({ "exposure_id": id, "duration_s": duration }),
            )
            .await;
        assert_eq!(answer.http_status, 400, "{duration}: {}", answer.envelope);
        assert_eq!(error_code(&answer), "INVALID_REQUEST");
        assert!(sent.is_empty(), "{duration}");
    }
    let payload = admin.enable(&id, ENABLE_MAX_SECS).await;
    assert_eq!(payload["exposure"]["enabled_until"], "2026-11-18T08:00:00Z");
    admin.enable(&id, ENABLE_MIN_SECS).await;

    let unknown = format!("{WEB_EXPOSURE_ID_PREFIX}{}", Uuid::new_v4());
    let (answer, sent) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": unknown, "duration_s": 3600 }),
        )
        .await;
    assert_eq!(answer.http_status, 404, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_NOT_FOUND");
    assert!(sent.is_empty());
    for malformed in [
        "exp:nope",
        "shop",
        "enb:6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b",
    ] {
        let (answer, _) = admin
            .cloud(
                "enable_exposure",
                json!({ "exposure_id": malformed, "duration_s": 3600 }),
            )
            .await;
        assert_eq!(answer.http_status, 400, "{malformed}: {}", answer.envelope);
    }
}

/// enable (2): no ILK for IO.web@motherbee (never launched, or identity unreadable): 409
/// IO_WEB_ABSENT, nothing put; the views say io.web is absent.
#[tokio::test(start_paused = true)]
async fn enable_answers_io_web_absent_without_its_ilk() {
    let mut admin = exposures_admin("web21-enable-absent", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let unread: [crate::NodeIlk; 2] = [
        Arc::new(|_: &Path, _: &str| Ok(None)),
        Arc::new(|_: &Path, _: &str| Err("the identity region is not mapped".to_string())),
    ];
    for node_ilk in unread {
        admin.ctx.node_ilk = node_ilk;
        let (answer, sent) = admin
            .cloud(
                "enable_exposure",
                json!({ "exposure_id": id, "duration_s": 3600 }),
            )
            .await;
        assert_eq!(answer.http_status, 409, "{}", answer.envelope);
        assert_eq!(error_code(&answer), "IO_WEB_ABSENT");
        assert!(
            answer.envelope["error_detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("run_node IO.web@motherbee")),
            "{}",
            answer.envelope
        );
        assert!(sent.is_empty());
        assert_eq!(admin.views()[0]["io_web"]["state"], "absent");
    }
    assert!(admin.on_disk().exposures[0].enablements.is_empty());
}

// ---------------------------------------------------------------------------------------------
// disable_exposure, unpublish_exposure (§5.1, §2.3)
// ---------------------------------------------------------------------------------------------

/// disable: a closing change acts first: io.web's set without the enablement is pushed while the
/// file still holds it, then written. A closed exposure is disabled again with nothing done.
#[tokio::test(start_paused = true)]
async fn disable_pushes_a_set_without_the_enablement_before_writing() {
    let mut admin = exposures_admin("web21-disable", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    let enabled = admin.on_disk();
    admin.mesh.on_disk_at_push.clear();

    let (answer, sent) = admin
        .cloud("disable_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    let payload = &answer.envelope["payload"];
    assert_eq!(payload["was_enabled"], true);
    assert_eq!(payload["persisted"], true);
    assert_eq!(payload["exposure"]["state"], "closed");
    assert_eq!(payload["exposure"]["io_web"]["state"], "acknowledged");
    assert_eq!(names_of(&sent), [MSG_IO_WEB_EXPOSURES], "no route changes");
    assert!(set_of(&sent[0]).exposures[0].enablements.is_empty());
    assert_eq!(
        admin.mesh.on_disk_at_push,
        vec![Some(enabled.clone())],
        "pushed before the write"
    );
    let disabled = admin.on_disk();
    assert!(disabled.exposures[0].enablements.is_empty());
    assert!(disabled.generation > enabled.generation);

    let (answer, sent) = admin
        .cloud("disable_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["was_enabled"], false);
    assert!(sent.is_empty());
    assert_eq!(admin.on_disk(), disabled);
}

/// §2.3: an opening change that cannot be written is refused, 500 RECORD_FAILED, nothing pushed;
/// a closing one is done all the same, `persisted: false`, memory holding it.
#[tokio::test(start_paused = true)]
async fn disable_answers_persisted_false_when_the_write_fails() {
    let mut admin = exposures_admin("web21-write-fails", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    // A directory where the file goes: no write lands.
    fs::remove_file(&admin.mesh.registry).expect("the file");
    fs::create_dir_all(admin.mesh.registry.join("in-the-way")).expect("a directory");

    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "docs", "upstream": { "kind": "archi" } }),
        )
        .await;
    assert_eq!(answer.http_status, 500, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "RECORD_FAILED");
    assert!(sent.is_empty());
    assert_eq!(admin.ctx.exposures.hosts(), [SHOP]);

    let (answer, sent) = admin
        .cloud("disable_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["persisted"], false);
    assert_eq!(answer.envelope["payload"]["exposure"]["state"], "closed");
    assert!(set_of(&sent[0]).exposures[0].enablements.is_empty());
    assert!(!admin.ctx.exposures.persisted());
    assert!(admin.ctx.exposures.file().exposures[0]
        .enablements
        .is_empty());

    // Tried again while the way is blocked; then written once it is clear.
    assert!(!admin.ctx.exposures.retry_persist());
    fs::remove_dir_all(&admin.mesh.registry).expect("the directory");
    assert!(admin.ctx.exposures.retry_persist());
    assert!(admin.ctx.exposures.persisted());
    assert_eq!(admin.on_disk(), admin.ctx.exposures.file());
    assert!(admin.on_disk().exposures[0].enablements.is_empty());
}

/// The loop (§2.3): at once and then every minute, it writes what a closing change left
/// unwritten, then pushes io.web's whole set, so an io.web that was stopped (kill_node) holds it
/// within a minute of its return.
#[tokio::test(start_paused = true)]
async fn the_io_web_loop_writes_what_a_closing_change_left_and_pushes_every_minute() {
    let mut admin = exposures_admin("web21-loop", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    // io.web is stopped, and the write fails: the disable is done all the same.
    admin.mesh.io_web = IoWebScript::Absent;
    fs::remove_file(&admin.mesh.registry).expect("the file");
    fs::create_dir_all(admin.mesh.registry.join("in-the-way")).expect("a directory");
    let (answer, _) = admin
        .cloud("disable_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.envelope["payload"]["persisted"], false);
    let generation = admin.ctx.exposures.file().generation;
    assert!(!admin.ctx.exposures.io_web_acknowledged(generation));

    // The way is clear and io.web is back.
    fs::remove_dir_all(&admin.mesh.registry).expect("the directory");
    admin.mesh.io_web = IoWebScript::Ready;
    let exposures = Arc::clone(&admin.ctx.exposures);
    let looping = tokio::spawn(
        Arc::clone(&exposures).run_io_web(Arc::clone(&admin.ctx.rpc), "motherbee".to_string()),
    );
    serve(&mut admin.harness, &mut admin.mesh).await;
    let sent = std::mem::take(&mut admin.mesh.sent);
    assert_eq!(names_of(&sent), [MSG_IO_WEB_EXPOSURES]);
    assert!(set_of(&sent[0]).exposures[0].enablements.is_empty());
    assert!(exposures.persisted());
    assert_eq!(admin.on_disk(), exposures.file());
    assert!(exposures.io_web_acknowledged(generation));

    tokio::time::advance(IO_WEB_PUSH_INTERVAL).await;
    serve(&mut admin.harness, &mut admin.mesh).await;
    let sent = std::mem::take(&mut admin.mesh.sent);
    assert_eq!(names_of(&sent), [MSG_IO_WEB_EXPOSURES], "a minute later");
    looping.abort();
}

/// unpublish: the exposure leaves memory, the edge's routes and io.web's set are pushed while the
/// file still holds it, then the file is written; its key stays in the vault. Unknown: 404.
#[tokio::test(start_paused = true)]
async fn unpublish_removes_the_route_then_writes_and_an_unknown_one_is_not_found() {
    let mut admin = exposures_admin("web21-unpublish", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    let published = admin.on_disk();
    admin.mesh.on_disk_at_push.clear();

    let (answer, sent) = admin
        .cloud("unpublish_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(
        answer.envelope["payload"],
        json!({
            "exposure_id": id,
            "host": SHOP,
            "removed": true,
            "route": { "state": "acknowledged" },
            "io_web": { "state": "acknowledged" },
            "persisted": true,
        })
    );
    assert_eq!(names_of(&sent), [MSG_EDGE_WEB_ROUTES, MSG_IO_WEB_EXPOSURES]);
    assert_eq!(
        sent_named(&sent, MSG_EDGE_WEB_ROUTES).payload,
        json!({ "routes": [] })
    );
    assert!(set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES))
        .exposures
        .is_empty());
    assert_eq!(admin.mesh.on_disk_at_push, vec![Some(published)]);
    assert!(admin.on_disk().exposures.is_empty());
    assert!(admin.views().is_empty());

    let (answer, sent) = admin
        .cloud("unpublish_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 404, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_NOT_FOUND");
    assert!(sent.is_empty());
}

// ---------------------------------------------------------------------------------------------
// issue_exposure_link (§5.1, §3.2)
// ---------------------------------------------------------------------------------------------

/// A closed exposure has no link: 409 EXPOSURE_CLOSED, io.web not asked; nor one whose window
/// ended. An unknown one: 404.
#[tokio::test(start_paused = true)]
async fn issue_link_is_refused_while_closed() {
    let mut admin = exposures_admin("web21-link-closed", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let (answer, sent) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_CLOSED");
    assert!(sent.is_empty());

    admin.enable(&id, 60).await;
    admin.advance(60);
    let (answer, sent) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(
        error_code(&answer),
        "EXPOSURE_CLOSED",
        "{}",
        answer.envelope
    );
    assert!(sent.is_empty());

    let unknown = format!("{WEB_EXPOSURE_ID_PREFIX}{}", Uuid::new_v4());
    let (answer, _) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": unknown }))
        .await;
    assert_eq!(answer.http_status, 404, "{}", answer.envelope);
}

/// §3.4: io.web mints links from the last enablement alone, while it is live. A renewal that ends
/// before the enablement it renewed closes the exposure at its own end: the view says `closed` and
/// a link answers 409 EXPOSURE_CLOSED, io.web not asked, while the first enablement still runs its
/// sessions; a disable still closes that one.
#[tokio::test(start_paused = true)]
async fn a_renewal_that_ends_first_closes_the_exposure_to_new_links() {
    let mut admin = exposures_admin("web21-renewal-ends-first", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 8 * 3_600).await;
    admin.advance(600);
    let renewal = admin.enable(&id, 3_600).await;
    assert_eq!(renewal["renewed"], true);
    assert_eq!(renewal["exposure"]["enabled_until"], "2026-11-18T01:10:00Z");

    // The renewal's end: the first enablement runs to 08:00, and the exposure is closed.
    admin.advance(3_600);
    let view = admin.views()[0].clone();
    assert_eq!(view["state"], "closed", "{view}");
    assert_eq!(view["enabled_until"], Value::Null, "{view}");
    let (answer, sent) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 409, "{}", answer.envelope);
    assert_eq!(error_code(&answer), "EXPOSURE_CLOSED");
    assert!(sent.is_empty(), "io.web is not asked");

    let (answer, sent) = admin
        .cloud("disable_exposure", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(answer.envelope["payload"]["was_enabled"], true);
    assert!(set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES)).exposures[0]
        .enablements
        .is_empty());
}

/// io.web mints the link of the current enablement; SY.admin answers it once, `expires_at` in RFC
/// 3339. io.web issuing none, or not answering: 503 IO_WEB_UNAVAILABLE, retryable, asked once.
#[tokio::test(start_paused = true)]
async fn issue_link_answers_io_web_s_url() {
    let mut admin = exposures_admin("web21-link", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    let file = admin.ctx.exposures.file();
    let current = file.exposures[0]
        .current(START_SECS)
        .expect("enabled")
        .clone();

    admin.mesh.answer_links(vec![link_ok(START_SECS + 120)]);
    let (answer, sent) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(
        answer.envelope["payload"],
        json!({
            "exposure_id": id,
            "url": format!("https://{SHOP}/.fluxbee/enter?code=c0de"),
            "expires_at": "2026-11-18T00:02:00Z",
            "link_id": "0123456789abcdef",
        })
    );
    assert_eq!(names_of(&sent), [MSG_IO_WEB_ACCESS_LINK]);
    assert_eq!(dst_of(&sent[0]), json!(IO_WEB));
    let asked: IoWebAccessLinkRequest =
        serde_json::from_value(sent[0].payload.clone()).expect("the request");
    assert_eq!(
        asked,
        IoWebAccessLinkRequest {
            exposure_id: id.clone(),
            enablement_id: current.enablement_id.clone(),
            generation: file.generation,
        }
    );

    for refusal in [
        link_refused(IO_WEB_ERROR_KEY_UNAVAILABLE),
        link_refused(IO_WEB_ERROR_UNAUTHORIZED),
        None,
    ] {
        admin.mesh.answer_links(vec![refusal.clone()]);
        let (answer, sent) = admin
            .cloud("issue_exposure_link", json!({ "exposure_id": id }))
            .await;
        assert_eq!(answer.http_status, 503, "{refusal:?}: {}", answer.envelope);
        assert_eq!(error_code(&answer), "IO_WEB_UNAVAILABLE");
        assert_eq!(answer.envelope["payload"]["retryable"], true);
        assert_eq!(names_of(&sent), [MSG_IO_WEB_ACCESS_LINK], "{refusal:?}");
    }
}

/// io.web's set is older than SY.admin's (ENABLEMENT_UNKNOWN, EXPOSURE_UNKNOWN): SY.admin pushes
/// the set and asks once more, never twice.
#[tokio::test(start_paused = true)]
async fn issue_link_pushes_once_on_enablement_unknown() {
    let mut admin = exposures_admin("web21-link-older", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;

    for older in [
        IO_WEB_ERROR_ENABLEMENT_UNKNOWN,
        IO_WEB_ERROR_EXPOSURE_UNKNOWN,
    ] {
        admin
            .mesh
            .answer_links(vec![link_refused(older), link_ok(START_SECS + 120)]);
        let (answer, sent) = admin
            .cloud("issue_exposure_link", json!({ "exposure_id": id }))
            .await;
        assert_eq!(answer.http_status, 200, "{older}: {}", answer.envelope);
        let order: Vec<&str> = sent
            .iter()
            .map(|message| message.meta.msg.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(
            order,
            [
                MSG_IO_WEB_ACCESS_LINK,
                MSG_IO_WEB_EXPOSURES,
                MSG_IO_WEB_ACCESS_LINK
            ],
            "{older}"
        );
    }
    admin.mesh.answer_links(vec![
        link_refused(IO_WEB_ERROR_EXPOSURE_UNKNOWN),
        link_refused(IO_WEB_ERROR_ENABLEMENT_UNKNOWN),
    ]);
    let (answer, sent) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 503, "{}", answer.envelope);
    assert_eq!(sent.len(), 3, "one push, two asks");
}

// ---------------------------------------------------------------------------------------------
// The registry (§2.3)
// ---------------------------------------------------------------------------------------------

/// The edge's routes carry every published host, sorted, and lose an unpublished one.
#[tokio::test(start_paused = true)]
async fn the_web_routes_carry_every_published_host() {
    let mut admin = exposures_admin("web21-routes", Some(EDGE));
    admin.publish("shop", shop_upstream()).await;
    let docs = admin
        .publish(
            "docs",
            json!({ "kind": "address", "address": "10.0.0.99", "port": 8443 }),
        )
        .await;
    let (_, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "archi", "upstream": { "kind": "archi" } }),
        )
        .await;
    let hosts = |message: &Message| -> Vec<String> {
        message.payload["routes"]
            .as_array()
            .expect("routes")
            .iter()
            .map(|route| route["host"].as_str().expect("a host").to_string())
            .collect()
    };
    let all = ["archi.hive-1.example.com", "docs.hive-1.example.com", SHOP];
    assert_eq!(hosts(sent_named(&sent, MSG_EDGE_WEB_ROUTES)), all);
    assert_eq!(admin.ctx.exposures.hosts(), all);
    let (_, sent) = admin
        .cloud("unpublish_exposure", json!({ "exposure_id": docs }))
        .await;
    assert_eq!(
        hosts(sent_named(&sent, MSG_EDGE_WEB_ROUTES)),
        ["archi.hive-1.example.com", SHOP]
    );
}

/// The generation is max(previous + 1, now in ms): it moves at every change, and a registry
/// whose file was lost starts past every set io.web took.
#[tokio::test(start_paused = true)]
async fn the_generation_never_goes_back_even_after_the_file_is_lost() {
    assert_eq!(next_generation(5, 3), 6);
    assert_eq!(next_generation(5, 100), 100);
    assert_eq!(next_generation(u64::MAX, 0), u64::MAX);

    let mut admin = exposures_admin("web21-generation", Some(EDGE));
    assert_eq!(admin.ctx.exposures.file().generation, START_MS);
    admin.publish("shop", shop_upstream()).await;
    admin.publish("docs", json!({ "kind": "archi" })).await;
    let taken = admin.on_disk().generation;
    assert_eq!(taken, START_MS + 2, "two changes in the same ms");

    // The file is lost; SY.admin starts again a second later.
    fs::remove_file(&admin.mesh.registry).expect("the file");
    admin.advance(1);
    admin.restart();
    assert!(admin.ctx.exposures.file().exposures.is_empty());
    assert!(admin.ctx.exposures.io_web_payload().generation > taken);
    let (_, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert!(set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES)).generation > taken);
}

/// §2.1: a host is its label under the main name SY.admin starts with. A registry written under
/// another main name loads with every host under the current one, so the edge's routes, io.web's
/// set and the certificate's names follow `admin.public_base_url`, and the next write keeps it.
/// With no main name no exposure's host is routed, pushed to io.web or ordered, and the registry
/// keeps the exposure for when one returns.
#[tokio::test(start_paused = true)]
async fn the_hosts_follow_the_main_name_at_each_start() {
    let mut admin = exposures_admin("web21-main-name-moves", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    assert_eq!(admin.ctx.exposures.hosts(), [SHOP]);

    // admin.public_base_url names another host, and SY.admin starts again.
    admin.ctx.public_base_url = Some("https://hive-2.example.com".to_string());
    admin.restart();
    let moved = "shop.hive-2.example.com";
    assert_eq!(admin.ctx.exposures.hosts(), [moved]);
    assert_eq!(
        admin.ctx.web.payload(),
        json!({ "routes": [{ "host": moved, "upstream": "io.web" }] })
    );
    assert_eq!(
        admin.ctx.exposures.io_web_payload().exposures[0].host,
        moved
    );
    assert_eq!(admin.views()[0]["host"], moved);
    admin.enable(&id, 60).await;
    assert_eq!(admin.on_disk().exposures[0].host, moved, "the next write");

    // No main name.
    admin.ctx.public_base_url = None;
    admin.restart();
    assert!(admin.ctx.exposures.hosts().is_empty());
    assert!(admin.ctx.exposures.io_web_payload().exposures.is_empty());
    assert_eq!(admin.ctx.exposures.file().exposures.len(), 1, "kept");
}

/// A file that cannot be read is kept as it is for the operator; the sets pushed are empty, and
/// every exposure write is refused, 500 RECORD_FAILED, with nothing sent.
#[tokio::test(start_paused = true)]
async fn an_unreadable_registry_is_kept_pushes_empty_sets_and_refuses_writes() {
    let mut admin = exposures_admin("web21-unreadable", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    let good = fs::read_to_string(&admin.mesh.registry).expect("the file");
    let other_schema = good.replace("\"schema_version\": 1", "\"schema_version\": 2");
    let not_an_id = good.replace(&id, "exp:shop");
    for broken in ["{ not json".to_string(), other_schema, not_an_id] {
        fs::write(&admin.mesh.registry, &broken).expect("broken");
        admin.restart();
        assert!(admin.ctx.exposures.unreadable().is_some(), "{broken}");
        assert_eq!(
            fs::read_to_string(&admin.mesh.registry).expect("kept"),
            broken
        );
        assert!(admin.ctx.exposures.hosts().is_empty());
        assert!(admin.ctx.exposures.io_web_payload().exposures.is_empty());
        for (action, params) in [
            (
                "publish_exposure",
                json!({ "label": "docs", "upstream": { "kind": "archi" } }),
            ),
            (
                "enable_exposure",
                json!({ "exposure_id": id, "duration_s": 3600 }),
            ),
            ("disable_exposure", json!({ "exposure_id": id })),
            ("unpublish_exposure", json!({ "exposure_id": id })),
        ] {
            let (answer, sent) = admin.cloud(action, params).await;
            assert_eq!(answer.http_status, 500, "{action}: {}", answer.envelope);
            assert_eq!(error_code(&answer), "RECORD_FAILED");
            assert!(sent.is_empty(), "{action}");
        }
        assert!(admin.views().is_empty());
        assert_eq!(
            fs::read_to_string(&admin.mesh.registry).expect("kept"),
            broken
        );
    }
}

/// A registry repaired in place keeps the generation it had, older than the empty sets SY.admin
/// pushed while it could not read it, and io.web refuses every push as STALE_GENERATION until
/// Fluxbee Cloud's next change. That answer is an ERROR, once, naming the cause and the remedy:
/// an unreadable registry is removed, never repaired. Another refusal stays a warning.
#[test]
fn a_stale_generation_answer_is_an_error_naming_a_repaired_registry() {
    let captured = crate::tests::LogCapture::start();
    let exposures = registry_with_hosts(&test_temp_dir("web21-stale-generation"), &[SHOP]);
    let stale = IoWebAnswer::Refused {
        code: IO_WEB_ERROR_STALE_GENERATION.to_string(),
        detail: "generation 1 is older than the one held".to_string(),
    };
    exposures.record_io_web(1, &stale);
    exposures.record_io_web(1, &stale);
    let text = captured.text();
    let errors: Vec<&str> = text.lines().filter(|line| line.contains("ERROR")).collect();
    assert_eq!(errors.len(), 1, "{text}");
    assert!(
        errors[0].contains("repaired or restored in place")
            && errors[0].contains("removed, never repaired"),
        "{}",
        errors[0]
    );
    exposures.record_io_web(
        2,
        &IoWebAnswer::Refused {
            code: IO_WEB_ERROR_UNAUTHORIZED.to_string(),
            detail: "not from SY.admin".to_string(),
        },
    );
    let text = captured.text();
    assert_eq!(
        text.lines().filter(|line| line.contains("ERROR")).count(),
        1
    );
    assert!(text.contains("WARN"), "{text}");
}

/// §6: io.web's answer to a set is kept, an acknowledgement at the current generation or a later
/// one making it `acknowledged` whatever the keys; no answer, or an older set's, `pending`; no
/// ILK, `absent`. An answer to a push a newer one overtook is not kept.
#[test]
fn io_web_s_answers_are_kept_and_an_overtaken_push_is_not() {
    let exposures = registry_with_hosts(&test_temp_dir("web21-io-web-answers"), &[SHOP]);
    let taken = |generation: u64, key: IoWebKeyState| IoWebAnswer::Taken {
        generation,
        changed: true,
        keys: vec![IoWebEnablementKey {
            enablement_id: format!("{WEB_ENABLEMENT_ID_PREFIX}{}", Uuid::new_v4()),
            key,
            detail: None,
        }],
    };
    assert!(!exposures.io_web_acknowledged(1), "nothing answered yet");
    exposures.record_io_web(10, &taken(10, IoWebKeyState::Unavailable));
    assert!(
        exposures.io_web_acknowledged(10),
        "taken, a key unavailable"
    );
    assert!(exposures.io_web_acknowledged(9));
    assert!(!exposures.io_web_acknowledged(11), "an older set");
    let status = exposures.io_web_status();
    assert_eq!(status["acknowledged_generation"], 10);
    assert_eq!(status["last_answer"]["state"], "taken");

    exposures.record_io_web(11, &IoWebAnswer::NoAnswer("no answer in 5s".to_string()));
    assert!(!exposures.io_web_acknowledged(10), "stopped: pending");
    assert_eq!(exposures.io_web_status()["acknowledged_generation"], 10);
    exposures.record_io_web(11, &IoWebAnswer::Absent("UNREACHABLE".to_string()));
    assert!(!exposures.io_web_acknowledged(10));
    assert_eq!(exposures.io_web_status()["last_answer"]["state"], "absent");

    // A push of 13 left before the answer to 12 came back: 12's answer is not kept.
    locked(&exposures.io_web).pushed_generation = Some(13);
    exposures.record_io_web(12, &taken(12, IoWebKeyState::Ready));
    assert_eq!(exposures.io_web_status()["last_answer"]["state"], "absent");
    exposures.record_io_web(13, &taken(13, IoWebKeyState::Ready));
    assert!(exposures.io_web_acknowledged(13));

    assert_eq!(io_web_state(true, true), "absent");
    assert_eq!(io_web_state(false, true), "acknowledged");
    assert_eq!(io_web_state(false, false), "pending");
}

/// io.web's commands go only through the exposure actions: sent as a plain message they would pass
/// as SY.admin's own.
#[tokio::test(start_paused = true)]
async fn send_node_message_never_carries_io_web_commands() {
    let mut admin = exposures_admin("web21-plain-message", Some(EDGE));
    for msg in ["IO_WEB_EXPOSURES", " io_web_access_link "] {
        let (status, body) = handle_send_node_message(
            &admin.ctx,
            &admin.ctx.rpc,
            json!({ "node_name": IO_WEB, "msg_type": "system", "msg": msg, "payload": {} }),
            None,
        )
        .await
        .expect("answered");
        let body: Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(status, 400, "{msg}: {body}");
        assert_eq!(body["error_code"], "INVALID_REQUEST");
        assert!(
            body["error_detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("exposure actions")),
            "{body}"
        );
    }
    assert!(admin
        .harness
        .next_outgoing_within(Duration::from_millis(50))
        .await
        .is_none());
}

/// FINDINGS A-352: a protected SYSTEM action or one of SY.vault's writes goes only through its own
/// action; sent as a plain message it would pass as SY.admin's own and skip that action's checks.
#[tokio::test(start_paused = true)]
async fn send_node_message_never_carries_a_protected_action_or_a_vault_write() {
    let mut admin = exposures_admin("a352-plain-message", Some(EDGE));
    for msg in [
        "VAULT_PUT",
        " vault_assign ",
        "VAULT_DELETE",
        "VAULT_ROLLBACK",
        "VAULT_ROTATE",
        "EDGE_WEB_ROUTES",
        "edge_tls_reload",
        "CONFIG_SET",
    ] {
        let (status, body) = handle_send_node_message(
            &admin.ctx,
            &admin.ctx.rpc,
            json!({ "node_name": IO_WEB, "msg_type": "system", "msg": msg, "payload": {} }),
            None,
        )
        .await
        .expect("answered");
        let body: Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(status, 400, "{msg}: {body}");
        assert_eq!(body["error_code"], "INVALID_REQUEST");
        assert!(
            body["error_detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("A-352")),
            "{body}"
        );
    }
    assert!(admin
        .harness
        .next_outgoing_within(Duration::from_millis(50))
        .await
        .is_none());
}

/// §5.1: one exposure write at a time. Another is answered at once, 409 BUSY and retryable, after
/// its field checks and before anything is read (the ILK) or sent. The link and the list take no
/// lock.
#[tokio::test(start_paused = true)]
async fn a_busy_exposure_lock_answers_busy_and_reads_nothing() {
    let mut admin = exposures_admin("web21-busy", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&reads);
    admin.ctx.node_ilk = Arc::new(move |_: &Path, node: &str| {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok((node == IO_WEB).then(|| IO_WEB_ILK.to_string()))
    });

    let exposures = Arc::clone(&admin.ctx.exposures);
    let one_write = exposures.try_lock().expect("free");
    for (action, params) in [
        (
            "publish_exposure",
            json!({ "label": "docs", "upstream": { "kind": "archi" } }),
        ),
        (
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 3600 }),
        ),
        ("disable_exposure", json!({ "exposure_id": id })),
        ("unpublish_exposure", json!({ "exposure_id": id })),
    ] {
        let (answer, sent) = admin.cloud(action, params).await;
        assert_eq!(answer.http_status, 409, "{action}: {}", answer.envelope);
        assert_eq!(error_code(&answer), "BUSY");
        assert_eq!(answer.envelope["payload"]["retryable"], true);
        assert!(sent.is_empty(), "{action}");
    }
    assert_eq!(reads.load(Ordering::SeqCst), 0, "nothing read");
    let (answer, _) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 0 }),
        )
        .await;
    assert_eq!(
        error_code(&answer),
        "INVALID_REQUEST",
        "the field checks first"
    );

    admin.mesh.answer_links(vec![link_ok(START_SECS + 120)]);
    let (answer, _) = admin
        .cloud("issue_exposure_link", json!({ "exposure_id": id }))
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(admin.views().len(), 1);

    drop(one_write);
    admin.publish("docs", json!({ "kind": "archi" })).await;
}

/// list_exposures: every exposure's view in publish order, with the count and the most a hive
/// holds; it takes no parameter. Archi and the HTTP API read it through the dispatcher.
#[tokio::test(start_paused = true)]
async fn list_exposures_answers_every_view_and_takes_no_parameter() {
    let mut admin = exposures_admin("web21-list", Some(EDGE));
    let empty = handle_list_exposures(&admin.ctx, &json!({}));
    assert_eq!(
        empty.envelope["payload"],
        json!({ "count": 0, "max": MAX_EXPOSURES, "exposures": [] })
    );
    let shop = admin.publish("shop", shop_upstream()).await;
    let archi = admin.publish("archi", json!({ "kind": "archi" })).await;
    admin.enable(&shop, 3600).await;

    for origin in [AdminOrigin::Mesh(ARCHI), AdminOrigin::Http] {
        let answer = dispatch_internal_admin_command(
            &admin.ctx,
            &admin.ctx.rpc,
            LIST_EXPOSURES,
            None,
            json!({}),
            origin,
        )
        .await
        .expect("answered");
        assert_eq!(answer.http_status, 200, "{origin:?}: {}", answer.envelope);
        let payload = &answer.envelope["payload"];
        assert_eq!(payload["count"], 2);
        assert_eq!(payload["max"], MAX_EXPOSURES);
        let views = payload["exposures"].as_array().expect("views");
        assert_eq!(views[0]["exposure_id"], shop);
        assert_eq!(views[0]["state"], "enabled");
        assert_eq!(views[0]["enabled_until"], "2026-11-18T01:00:00Z");
        assert_eq!(views[1]["exposure_id"], archi);
        assert_eq!(views[1]["state"], "closed");
        assert_eq!(views[1]["upstream"], json!({ "kind": "archi" }));
        for view in views {
            for field in [
                "exposure_id",
                "label",
                "host",
                "url",
                "upstream",
                "state",
                "enabled_until",
                "published_at",
                "certificate",
                "route",
                "io_web",
            ] {
                assert!(view.get(field).is_some(), "{field}: {view}");
            }
        }
    }
    let refused = handle_list_exposures(&admin.ctx, &json!({ "label": "shop" }));
    assert_eq!(refused.http_status, 400, "{}", refused.envelope);
    assert!(admin
        .harness
        .next_outgoing_within(Duration::from_millis(50))
        .await
        .is_none());
}

// ---------------------------------------------------------------------------------------------
// Cloud's origins (docs/io-web-v2-stage-2-2.md §2)
// ---------------------------------------------------------------------------------------------

/// The origin of Cloud's page, and a second one as while it moves (io-web-v2 §13.7).
const CLOUD_PAGE: &str = "https://cloud.fluxbee.ai";
const CLOUD_ROOT: &str = "https://fluxbee.ai:8443";

/// Both pushes carry Cloud's origins, whoever sends them: the run of publish_cloud_endpoint that
/// names them (`keep_cloud_origins`, both at once), then every exposure change and every loop. The
/// edge's routes carry them as `frame_origins` only while there is one, so an edge before 2.2,
/// which refuses a field it does not know, takes every set while none is named, and refuses one
/// that names them (why every edge runs 2.2 before the first origin); io.web's set carries them as
/// `cloud_origins`, absent on the wire while empty. The `frame_origins` the edge's acknowledgement
/// names are kept beside its hosts, past a refusal, and `GET /web/status` shows them with the
/// origins SY.admin holds.
#[tokio::test(start_paused = true)]
async fn the_pushes_carry_the_origins_and_the_edge_gets_none_while_empty() {
    let mut admin = exposures_admin("web22-origins-pushes", Some(EDGE));
    let shop_route = json!([{ "host": SHOP, "upstream": "io.web" }]);
    // None named: neither push names the field, and an edge before 2.2 takes the routes.
    admin.mesh.edge_before_2_2 = true;
    let (answer, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "shop", "upstream": shop_upstream() }),
        )
        .await;
    assert_eq!(answer.http_status, 200, "{}", answer.envelope);
    assert_eq!(
        answer.envelope["payload"]["exposure"]["route"]["state"],
        "acknowledged"
    );
    assert_eq!(
        sent_named(&sent, MSG_EDGE_WEB_ROUTES).payload,
        json!({ "routes": shop_route })
    );
    let set = &sent_named(&sent, MSG_IO_WEB_EXPOSURES).payload;
    assert!(set.get("cloud_origins").is_none(), "{set}");
    assert_eq!(
        admin.ctx.web.status()["acknowledged_frame_origins"],
        json!([])
    );

    // Named: both pushes at once, after the write.
    admin.mesh.edge_before_2_2 = false;
    admin.mesh.on_disk_at_push.clear();
    let origins = json!([CLOUD_PAGE, CLOUD_ROOT]);
    let (written, sent) = admin.keep_origins(&[CLOUD_PAGE, CLOUD_ROOT]).await;
    assert_eq!(written, Ok(()));
    assert_eq!(names_of(&sent), [MSG_EDGE_WEB_ROUTES, MSG_IO_WEB_EXPOSURES]);
    assert_eq!(
        sent_named(&sent, MSG_EDGE_WEB_ROUTES).payload,
        json!({ "routes": shop_route, "frame_origins": origins })
    );
    let set = set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES));
    assert_eq!(set.cloud_origins, [CLOUD_PAGE, CLOUD_ROOT]);
    assert_eq!(set, admin.ctx.exposures.io_web_payload());
    assert_eq!(admin.mesh.on_disk_at_push, vec![Some(admin.on_disk())]);
    assert_eq!(
        admin.ctx.web.status()["acknowledged_frame_origins"],
        origins
    );
    let status = admin.web_status().await;
    assert_eq!(status["web"]["cloud_origins"], origins, "{status}");
    assert_eq!(
        status["routes"]["acknowledged_frame_origins"], origins,
        "{status}"
    );

    // Every later push carries them: an enable's set, a publish's routes and set, the loops'.
    let id = admin.ctx.exposures.file().exposures[0].exposure_id.clone();
    let (_, sent) = admin
        .cloud(
            "enable_exposure",
            json!({ "exposure_id": id, "duration_s": 3600 }),
        )
        .await;
    assert_eq!(
        set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES)).cloud_origins,
        [CLOUD_PAGE, CLOUD_ROOT]
    );
    let (_, sent) = admin
        .cloud(
            "publish_exposure",
            json!({ "label": "docs", "upstream": { "kind": "archi" } }),
        )
        .await;
    assert_eq!(
        sent_named(&sent, MSG_EDGE_WEB_ROUTES).payload["frame_origins"],
        origins
    );
    assert_eq!(
        set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES)).cloud_origins,
        [CLOUD_PAGE, CLOUD_ROOT]
    );
    assert_eq!(admin.ctx.web.payload()["frame_origins"], origins);
    let exposures = Arc::clone(&admin.ctx.exposures);
    let looping = tokio::spawn(
        Arc::clone(&exposures).run_io_web(Arc::clone(&admin.ctx.rpc), "motherbee".to_string()),
    );
    serve(&mut admin.harness, &mut admin.mesh).await;
    let sent = std::mem::take(&mut admin.mesh.sent);
    assert_eq!(
        set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES)).cloud_origins,
        [CLOUD_PAGE, CLOUD_ROOT],
        "the loop's push"
    );
    looping.abort();

    // An edge before 2.2 refuses a set that names them and keeps its last set; what the edge
    // acknowledged last is kept.
    admin.mesh.edge_before_2_2 = true;
    let docs = admin.ctx.exposures.file().exposures[1].exposure_id.clone();
    let (answer, _) = admin
        .cloud("unpublish_exposure", json!({ "exposure_id": docs }))
        .await;
    assert_eq!(answer.envelope["payload"]["route"]["state"], "pending");
    let routes = admin.ctx.web.status();
    assert_eq!(routes["state"], "refused", "{routes}");
    assert!(
        routes["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("unknown field `frame_origins`")),
        "{routes}"
    );
    assert_eq!(routes["acknowledged_frame_origins"], origins, "{routes}");

    // Cleared: neither push names the field again, and an edge before 2.2 takes the set.
    let (written, sent) = admin.keep_origins(&[]).await;
    assert_eq!(written, Ok(()));
    assert_eq!(
        sent_named(&sent, MSG_EDGE_WEB_ROUTES).payload,
        json!({ "routes": shop_route })
    );
    let set = &sent_named(&sent, MSG_IO_WEB_EXPOSURES).payload;
    assert!(set.get("cloud_origins").is_none(), "{set}");
    let routes = admin.ctx.web.status();
    assert_eq!(routes["state"], "acknowledged", "{routes}");
    assert_eq!(routes["acknowledged_frame_origins"], json!([]), "{routes}");
    assert_eq!(admin.web_status().await["web"]["cloud_origins"], json!([]));
}

/// A change of Cloud's origins alone moves the registry's generation, as an exposure change does,
/// so io.web, which acknowledges a set of the generation it holds without reading it, reads the
/// new origins: the exposures as they were, the origins new, at a generation past the one io.web
/// holds, in the file before the pushes leave. Named again as they are, they are written again
/// with a new generation, as a run that retries a failed write does. A file written before 2.2
/// names none: it loads with none, its schema still 1, and the next write names them.
#[tokio::test(start_paused = true)]
async fn a_change_of_the_origins_alone_gives_a_new_generation() {
    let mut admin = exposures_admin("web22-origins-generation", Some(EDGE));
    let id = admin.publish("shop", shop_upstream()).await;
    admin.enable(&id, 3600).await;
    let before = admin.on_disk();
    let set_before = admin.ctx.exposures.io_web_payload();
    assert!(admin.ctx.exposures.io_web_acknowledged(before.generation));
    admin.mesh.on_disk_at_push.clear();

    let (written, sent) = admin.keep_origins(&[CLOUD_PAGE]).await;
    assert_eq!(written, Ok(()));
    let after = admin.on_disk();
    assert!(after.generation > before.generation, "a new generation");
    assert_eq!(
        after.exposures, before.exposures,
        "the exposures as they were"
    );
    assert_eq!(after.cloud_origins, [CLOUD_PAGE]);
    assert_eq!(after, admin.ctx.exposures.file());
    let set = set_of(sent_named(&sent, MSG_IO_WEB_EXPOSURES));
    assert_eq!(set.generation, after.generation);
    assert_eq!(set.exposures, set_before.exposures);
    assert_eq!(set.cloud_origins, [CLOUD_PAGE]);
    assert_eq!(
        admin.mesh.on_disk_at_push,
        vec![Some(after.clone())],
        "written before the push"
    );
    assert!(admin.ctx.exposures.io_web_acknowledged(after.generation));

    let (written, _) = admin.keep_origins(&[CLOUD_PAGE]).await;
    assert_eq!(written, Ok(()));
    let again = admin.on_disk();
    assert!(again.generation > after.generation, "written again");
    assert_eq!(again.cloud_origins, [CLOUD_PAGE]);

    // A file written before 2.2: no cloud_origins.
    let mut file: Value =
        serde_json::from_slice(&fs::read(&admin.mesh.registry).expect("the file")).expect("JSON");
    file.as_object_mut()
        .expect("an object")
        .remove("cloud_origins");
    fs::write(
        &admin.mesh.registry,
        serde_json::to_vec_pretty(&file).expect("serializes"),
    )
    .expect("written");
    admin.restart();
    assert_eq!(admin.ctx.exposures.unreadable(), None);
    let loaded = admin.ctx.exposures.file();
    assert_eq!(loaded.schema_version, 1);
    assert!(loaded.cloud_origins.is_empty());
    assert_eq!(loaded.exposures, again.exposures);
    assert!(admin
        .ctx
        .exposures
        .io_web_payload()
        .cloud_origins
        .is_empty());
    let (written, _) = admin.keep_origins(&[CLOUD_ROOT]).await;
    assert_eq!(written, Ok(()));
    assert_eq!(admin.on_disk().cloud_origins, [CLOUD_ROOT]);
}
