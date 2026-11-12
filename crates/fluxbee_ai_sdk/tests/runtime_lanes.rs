//! The AI runtime's lanes through `NodeRuntime::run_with_config`, with a router of the test's own on
//! a Unix socket (FINDINGS A-257): with every worker on an LLM turn, a status request and a
//! frontdesk handoff are answered at once, and a turn past the queue is answered `ai_runtime_busy`
//! while the ones that wait keep their order. Through the SDK's own sender: an answer over the
//! frame limit fails its message alone (FINDINGS A-294), and the node's status reads the load of
//! the runtime it was given (`NodeRuntime::with_load`, FINDINGS A-290).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    build_reply_message_runtime_src, AiNode, Message, NodeRuntime, Result, RuntimeConfig,
    RuntimeLoad, AI_RUNTIME_CHANNEL, AI_RUNTIME_REPLY_TOO_LARGE, AI_RUNTIME_SATURATED,
};
use fluxbee_sdk::managed_control_plane::{
    build_node_status_response_payload, ManagedNodeLifecycleState,
};
use fluxbee_sdk::protocol::{
    build_announce, Destination, Meta, NodeAnnouncePayload, NodeHelloPayload, Routing,
};
use fluxbee_sdk::socket::connection::{read_frame, write_frame};
use fluxbee_sdk::{
    NodeConfig, NodeUuidMode, OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher,
};
use serde_json::json;
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::{sleep, timeout};

/// Far longer than an answer that comes at once takes, on a busy machine too; one that waits
/// behind an LLM turn never comes in it, since the turns run until the test lets them go.
const WAIT: Duration = Duration::from_secs(10);

/// What the frontdesk is to its runtime: an LLM turn holds its worker until the test lets one go
/// (the earliest first), and a status request or a handoff is answered at once, in code: its
/// status as the runners answer it, the managed answer with its runtime's load. A turn whose trace
/// id starts with `big` is answered with 200,000 bytes, more than a frame carries. It notes the
/// order its handler starts each message in, by trace id.
struct Frontdesk {
    started: Mutex<Vec<String>>,
    turns: Semaphore,
    load: RuntimeLoad,
}

#[derive(Clone)]
struct Node(Arc<Frontdesk>);

#[async_trait]
impl AiNode for Node {
    async fn on_message(&self, msg: Message) -> Result<Option<Message>> {
        self.0
            .started
            .lock()
            .unwrap()
            .push(msg.routing.trace_id.clone());
        if msg.meta.msg_type == "system" {
            let mut status =
                build_node_status_response_payload(ManagedNodeLifecycleState::Configured, None);
            self.0.load.add_to_status(&mut status);
            let mut answer = build_reply_message_runtime_src(&msg, status);
            answer.meta.msg = Some("NODE_STATUS_GET_RESPONSE".to_string());
            return Ok(Some(answer));
        }
        if msg.meta.msg.as_deref() == Some("frontdesk_handoff") {
            return Ok(Some(build_reply_message_runtime_src(
                &msg,
                json!({"type": "frontdesk_result", "result_code": "REGISTERED"}),
            )));
        }
        self.0.turns.acquire().await.unwrap().forget();
        let content = if msg.routing.trace_id.starts_with("big") {
            "x".repeat(200_000)
        } else {
            msg.routing.trace_id.clone()
        };
        Ok(Some(build_reply_message_runtime_src(
            &msg,
            json!({"type": "text", "content": content}),
        )))
    }
}

/// The router the node connects to: the test sends it messages and reads its answers.
struct Router {
    dir: PathBuf,
    node: Arc<Frontdesk>,
    to_node: OwnedWriteHalf,
    answers: mpsc::UnboundedReceiver<Message>,
}

impl Router {
    /// Starts the frontdesk on the AI runtime with `config`, connected to this router.
    async fn start(config: RuntimeConfig) -> Self {
        // One directory per router, and a short path: a socket's path has about 100 bytes.
        static ROUTERS: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ai-lanes-{}-{}-{nanos}",
            std::process::id(),
            ROUTERS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hive.yaml"), "hive_id: motherbee\n").unwrap();
        let socket = dir.join("router.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let load = RuntimeLoad::new();
        let node = Arc::new(Frontdesk {
            started: Mutex::new(Vec::new()),
            turns: Semaphore::new(0),
            load: load.clone(),
        });
        let node_config = NodeConfig {
            name: "SY.frontdesk.gov@motherbee".to_string(),
            router_socket: socket,
            uuid_persistence_dir: dir.clone(),
            uuid_mode: NodeUuidMode::Ephemeral,
            config_dir: dir.clone(),
            version: "test".to_string(),
        };
        let runtime_node = Node(node.clone());
        tokio::spawn(async move {
            let profile = OperationalRouteProfile::builder()
                .command_channel(AI_RUNTIME_CHANNEL)
                .post_pending_rule(RouteMatch::Any, RouteTarget::Command(AI_RUNTIME_CHANNEL))
                .build()
                .expect("the runtime's profile");
            let dispatcher = RouterDispatcher::connect_with_retry(
                node_config,
                Duration::from_millis(50),
                profile,
            )
            .await
            .expect("the node connects");
            NodeRuntime::new(dispatcher, runtime_node)
                .with_load(load)
                .run_with_config(config)
                .await
        });

        let (stream, _) = timeout(WAIT, listener.accept())
            .await
            .expect("the node connects in time")
            .unwrap();
        let (mut from_node, mut to_node) = stream.into_split();
        let hello = read_frame(&mut from_node)
            .await
            .unwrap()
            .expect("the node's HELLO");
        let hello: Message = serde_json::from_slice(&hello).unwrap();
        let hello: NodeHelloPayload = serde_json::from_value(hello.payload).unwrap();
        let announce = build_announce(
            "uuid-of-the-router",
            &hello.uuid,
            "trace-announce",
            NodeAnnouncePayload {
                uuid: hello.uuid.clone(),
                name: hello.name,
                status: "ok".to_string(),
                vpn_id: 0,
                router_name: "RT.gateway@motherbee".to_string(),
            },
        );
        write_frame(&mut to_node, &serde_json::to_vec(&announce).unwrap())
            .await
            .unwrap();

        let (answered, answers) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut from_node).await {
                let _ = answered.send(serde_json::from_slice::<Message>(&frame).unwrap());
            }
        });
        Self {
            dir,
            node,
            to_node,
            answers,
        }
    }

    async fn send(&mut self, msg: Message) {
        write_frame(&mut self.to_node, &serde_json::to_vec(&msg).unwrap())
            .await
            .unwrap();
    }

    async fn answer(&mut self) -> Message {
        timeout(WAIT, self.answers.recv())
            .await
            .expect("an answer at once")
            .expect("the node is connected")
    }

    /// Waits until the node has started `count` messages.
    async fn started(&self, count: usize) -> Vec<String> {
        for _ in 0..1_000 {
            let started = self.node.started.lock().unwrap().clone();
            if started.len() >= count {
                return started;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the node started {:?}",
            self.node.started.lock().unwrap().clone()
        );
    }

    fn let_turns_go(&self, count: usize) {
        self.node.turns.add_permits(count);
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn message(id: &str, sender: &str, msg_type: &str, msg: Option<&str>) -> Message {
    Message {
        routing: Routing {
            src: format!("uuid-of-{sender}"),
            src_l2_name: Some(sender.to_string()),
            dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
            ttl: 16,
            trace_id: id.to_string(),
        },
        meta: Meta {
            msg_type: msg_type.to_string(),
            msg: msg.map(str::to_string),
            ..Meta::default()
        },
        payload: json!({}),
    }
}

/// A person's turn of a conversation, from their IO node.
fn turn(id: &str) -> Message {
    let mut msg = message(id, "IO.slack.main@motherbee", "user", None);
    msg.meta.thread_id = Some(format!("thread-{id}"));
    msg.payload = json!({"type": "text", "content": id});
    msg
}

fn status_request(id: &str) -> Message {
    message(
        id,
        "SY.orchestrator@motherbee",
        "system",
        Some("NODE_STATUS_GET"),
    )
}

/// io.api's handoff of a person to the frontdesk (`request_frontdesk_handoff`).
fn handoff(id: &str) -> Message {
    let mut msg = message(
        id,
        "IO.api.shop@motherbee",
        "user",
        Some("frontdesk_handoff"),
    );
    msg.payload = json!({
        "type": "frontdesk_handoff",
        "schema_version": 1,
        "operation": "complete_registration",
        "subject": {"display_name": "Ana", "email": "ana@example.com"}
    });
    msg
}

/// FINDINGS A-257: the frontdesk runs the default runtime, 4 workers. With 4 LLM turns in flight,
/// the orchestrator's status request and io.api's handoff are answered at once, where they waited
/// for a turn to end, past io.api's 10 s.
#[tokio::test]
async fn a_status_request_and_a_handoff_are_answered_while_four_turns_run() {
    let mut router = Router::start(RuntimeConfig::default()).await;
    for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
        router.send(turn(id)).await;
    }
    assert_eq!(
        router.started(4).await,
        ["turn-1", "turn-2", "turn-3", "turn-4"]
    );

    router.send(status_request("status")).await;
    router.send(handoff("handoff")).await;
    let mut answered = vec![
        router.answer().await.routing.trace_id,
        router.answer().await.routing.trace_id,
    ];
    answered.sort_unstable();
    assert_eq!(answered, ["handoff", "status"]);

    // The turns were never held up, and end in their order.
    for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
        router.let_turns_go(1);
        assert_eq!(router.answer().await.routing.trace_id, id);
    }
}

/// The bound on what waits: with its one worker busy and its queue of 2 full, the runtime answers
/// a turn `ai_runtime_busy` at once and never runs it; the turns that wait run in their order.
#[tokio::test]
async fn a_turn_past_the_queue_is_answered_busy_and_the_waiting_ones_keep_their_order() {
    let mut router = Router::start(RuntimeConfig {
        worker_pool_size: 1,
        queue_capacity: 2,
        ..RuntimeConfig::default()
    })
    .await;
    for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
        router.send(turn(id)).await;
    }

    let busy = router.answer().await;
    assert_eq!(busy.routing.trace_id, "turn-4");
    assert_eq!(busy.payload["type"], json!("error"));
    assert_eq!(busy.payload["code"], json!("ai_runtime_busy"));
    assert_eq!(busy.payload["retryable"], json!(true));
    assert_eq!(busy.meta.thread_id.as_deref(), Some("thread-turn-4"));

    router.let_turns_go(3);
    let mut answered = Vec::new();
    for _ in 0..3 {
        answered.push(router.answer().await.routing.trace_id);
    }
    assert_eq!(answered, ["turn-1", "turn-2", "turn-3"]);
    assert_eq!(
        router.started(3).await,
        ["turn-1", "turn-2", "turn-3"],
        "the turn answered busy never ran"
    );
}

/// FINDINGS A-294: an answer over the frame limit, refused by the SDK's sender before it goes,
/// fails its message alone: its sender gets the error that goes in its place, and the turn that
/// waited behind it runs and is answered. Before, the refusal stopped the runtime, and the node's
/// process with it.
#[tokio::test]
async fn an_answer_over_the_frame_limit_fails_that_message_alone() {
    let mut router = Router::start(RuntimeConfig {
        worker_pool_size: 1,
        ..RuntimeConfig::default()
    })
    .await;
    router.send(turn("big-1")).await;
    router.send(turn("turn-2")).await;
    router.let_turns_go(2);

    let stand_in = router.answer().await;
    assert_eq!(stand_in.routing.trace_id, "big-1");
    assert_eq!(stand_in.payload["type"], json!("error"));
    assert_eq!(stand_in.payload["code"], json!(AI_RUNTIME_REPLY_TOO_LARGE));
    assert_eq!(stand_in.payload["retryable"], json!(false));
    assert_eq!(stand_in.meta.thread_id.as_deref(), Some("thread-big-1"));

    let next = router.answer().await;
    assert_eq!(next.routing.trace_id, "turn-2");
    assert_eq!(next.payload["content"], json!("turn-2"));
    assert_eq!(router.started(2).await, ["big-1", "turn-2"]);
}

/// FINDINGS A-290: a node's status reads the load of the runtime it gave its handle to: with its
/// one worker busy and no queue, a turn is refused, and the status the node answers at once says
/// `DEGRADED`, why, and the load.
#[tokio::test]
async fn the_status_reads_the_saturated_runtime_it_was_given() {
    let mut router = Router::start(RuntimeConfig {
        worker_pool_size: 1,
        queue_capacity: 0,
        ..RuntimeConfig::default()
    })
    .await;
    router.send(status_request("status-before")).await;
    let before = router.answer().await;
    assert_eq!(before.payload["health_state"], json!("HEALTHY"));
    assert_eq!(
        before.payload["extensions"]["ai_runtime"]["state"],
        json!("serving")
    );

    router.send(turn("turn-1")).await;
    assert_eq!(router.started(2).await, ["status-before", "turn-1"]);
    router.send(turn("turn-2")).await;
    assert_eq!(
        router.answer().await.payload["code"],
        json!("ai_runtime_busy")
    );

    router.send(status_request("status")).await;
    let status = router.answer().await;
    assert_eq!(status.routing.trace_id, "status");
    assert_eq!(status.payload["health_state"], json!("DEGRADED"));
    assert_eq!(
        status.payload["last_error"]["code"],
        json!(AI_RUNTIME_SATURATED)
    );
    let load = &status.payload["extensions"]["ai_runtime"];
    assert_eq!(load["state"], json!("saturated"));
    assert_eq!(load["model_workers"], json!(1));
    assert_eq!(load["model_workers_busy"], json!(1));
    assert_eq!(load["turns_refused"], json!(1));

    router.let_turns_go(1);
    assert_eq!(router.answer().await.routing.trace_id, "turn-1");
}
