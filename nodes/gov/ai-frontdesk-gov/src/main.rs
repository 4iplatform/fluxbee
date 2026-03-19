use fluxbee_sdk::protocol::Meta;
use fluxbee_sdk::{connect, NodeReceiver, NodeSender};
use gov_common::{build_node_config, env_opt, env_or};
use serde_json::Value;
use tokio::time::{timeout, Duration};
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or("JSR_LOG_LEVEL", "info")))
        .init();

    let cfg = build_node_config("AI.frontdesk.gov", "0.1.0");
    let identity_target = env_opt("GOV_IDENTITY_TARGET").unwrap_or_else(|| "SY.identity".into());

    tracing::info!(
        node = %cfg.name,
        version = %cfg.version,
        identity_target = %identity_target,
        "starting AI.frontdesk.gov scaffold"
    );

    let (sender, mut receiver) = connect(&cfg).await?;
    run_loop(&sender, &mut receiver, &identity_target).await?;
    Ok(())
}

async fn run_loop(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    identity_target: &str,
) -> Result<(), DynError> {
    loop {
        let msg = match timeout(Duration::from_secs(300), receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => continue,
        };

        let src_ilk = src_ilk_from_meta(&msg.meta).unwrap_or_default();
        let kind = msg.meta.msg_type.as_str();

        tracing::info!(
            trace_id = %msg.routing.trace_id,
            src = %msg.routing.src,
            msg_type = %kind,
            src_ilk = %src_ilk,
            "frontdesk.gov received message"
        );

        if !src_ilk.starts_with("ilk:") {
            continue;
        }

        // TODO(frontdesk.gov):
        // 1) identificar/validar persona
        // 2) resolver tenant objetivo
        // 3) llamar ILK_REGISTER via fluxbee_sdk::identity::identity_system_call_ok
        // 4) si corresponde, llamar ILK_ADD_CHANNEL con merge_from_ilk_id
        // 5) manejar errores de negocio (INVALID_TENANT, TENANT_PENDING, DUPLICATE_EMAIL, ...)
        tracing::debug!(
            target = %identity_target,
            src_ilk = %src_ilk,
            "registration workflow placeholder (not implemented)"
        );

        // Placeholder para evitar warning de parámetro no usado en scaffold inicial.
        let _ = sender;
    }
}

fn src_ilk_from_meta(meta: &Meta) -> Option<String> {
    meta.src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}
