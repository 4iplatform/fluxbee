//! First-contact gate: hand an unregistered sender to AI.frontdesk before relaying.
//!
//! # Por que existe
//!
//! Cuando alguien escribe por primera vez desde un canal externo, el nodo IO le crea un ILK con
//! `registration_status = "temporary"` e `identification: {}` — un handle vacio para "alguien que
//! escribio y todavia no sabemos quien es". La spec (`docs/10-identity-v2.md`) dice que ese ILK lo
//! asciende **AI.frontdesk**, el unico autorizado junto a SY.orchestrator a emitir `ILK_REGISTER`.
//!
//! Pero nada llevaba al desconocido hasta alla, asi que el ILK quedaba `temporary` para siempre.
//!
//! # Que hace, y que NO hace
//!
//! Replica el patron de io.api (`requires_frontdesk_intermediate` en `io-api/src/main.rs`):
//! **gate-then-forward**. Se le manda al frontdesk un payload ESTRUCTURADO —el camino determinista,
//! el mismo que ya corre en produccion— y si eso sale bien, el mensaje original **sigue viaje a su
//! destino real**. El mensaje del usuario no se pierde ni se lo obliga a reescribirlo.
//!
//! Deliberadamente NO usa el camino conversacional por LLM del frontdesk: hoy es codigo muerto y
//! ademas defectuoso (sin `thread_state` responde `REGISTERED`/`complete` sin haber llamado a
//! `ILK_REGISTER`). Encenderlo es una decision aparte.
//!
//! # Apagado por defecto
//!
//! `FrontdeskGateConfig::default()` viene con `enabled: false`, y el nodo se comporta exactamente
//! como antes. La maquinaria queda instalada y se enciende por CONFIG_SET cuando el lado del
//! frontdesk este resuelto.
//!
//! # Lo que este modulo NO resuelve, a proposito
//!
//! El router YA tiene el desvio equivalente (`apply_identity_pre_resolve`: si el emisor es
//! `temporary`, fuerza destino al frontdesk) y corta antes de OPA — no necesita politica Rego. Pero
//! es inalcanzable para io.slack/io.wapp porque, al tener `dst_node` configurado, emiten
//! `Destination::Unicast` y nunca pasan por el pre-resolve. Migrar a `Destination::Resolve` seria
//! el diseño previsto, pero hoy NO hay ninguna politica OPA de usuario cargada: cualquier mensaje
//! que el pre-resolve no atrape caeria en `OpaError::NotLoaded` y moriria. Por eso la compuerta va
//! en el nodo, que es determinista y no depende de OPA.

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use serde_json::{json, Value};

use crate::frontdesk_contract::{FrontdeskHandoffPayload, FrontdeskHandoffSubject};

/// Nombre por defecto del registrador. Es el mismo que usa io.api y el que el router lee de
/// `government.identity_frontdesk`.
pub const DEFAULT_FRONTDESK_TARGET: &str = "SY.frontdesk.gov@motherbee";

/// El tipo y la operacion del payload estructurado, tal como los espera el frontdesk.
pub const FRONTDESK_HANDOFF_TYPE: &str = "frontdesk_handoff";
pub const FRONTDESK_OPERATION_REGISTER: &str = "complete_registration";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontdeskGateConfig {
    /// APAGADA por defecto. Encenderla cambia el comportamiento de un hive vivo.
    pub enabled: bool,
    pub target: String,
}

impl Default for FrontdeskGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target: DEFAULT_FRONTDESK_TARGET.to_string(),
        }
    }
}

impl FrontdeskGateConfig {
    /// Lee `io.frontdesk.{enabled,target}` de la config efectiva del nodo.
    ///
    /// Ausencia total => apagada. Un `target` vacio o en blanco NO enciende nada: cae al default,
    /// porque una compuerta encendida apuntando a "" mandaria el primer contacto a la nada.
    pub fn from_effective_config(effective: Option<&Value>) -> Self {
        let node = effective
            .and_then(|c| c.get("io"))
            .and_then(|io| io.get("frontdesk"));
        let enabled = node
            .and_then(|f| f.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let target = node
            .and_then(|f| f.get("target"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_FRONTDESK_TARGET)
            .to_string();
        Self { enabled, target }
    }
}

/// True cuando el emisor todavia no esta registrado y hay que pasarlo por el frontdesk.
///
/// `None` (no pudimos leer el status) NO abre la compuerta: sin saber, se entrega como siempre. La
/// alternativa —frenar todo lo que no podemos clasificar— convierte una lectura fallida de SHM en
/// una caida del canal.
pub fn gate_required(registration_status: Option<&str>) -> bool {
    registration_status
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(|status| !status.eq_ignore_ascii_case("complete"))
}

/// Lee el `registration_status` del emisor desde el SHM de identidad.
///
/// Misma via que usa io.api (`resolve_identity_option_from_hive_id`). Devuelve `None` si no se
/// puede resolver — y por `gate_required`, eso significa "entregar normal".
pub fn sender_registration_status(
    hive_id: &str,
    channel_type: &str,
    address: &str,
    tenant_id: &str,
) -> Option<String> {
    if hive_id.is_empty() {
        return None;
    }
    match fluxbee_sdk::resolve_identity_option_from_hive_id(
        hive_id,
        channel_type,
        address,
        tenant_id,
    ) {
        Ok(Some(option)) => Some(option.ilk.registration_status),
        Ok(None) => None,
        Err(err) => {
            tracing::debug!(
                channel_type = %channel_type,
                error = %err,
                "no se pudo leer el registration_status del emisor; se entrega sin pasar por el frontdesk"
            );
            None
        }
    }
}

/// Arma el mensaje de handoff estructurado para el frontdesk.
///
/// Los campos de `FrontdeskHandoffSubject` son todos opcionales, asi que un canal que solo conoce
/// un user id externo puede igual presentar al sujeto: va en `attributes`, y el frontdesk sabe que
/// tiene que averiguar el resto.
pub fn build_handoff_message(
    node_uuid: &str,
    cfg: &FrontdeskGateConfig,
    src_ilk: &str,
    tenant_id: Option<&str>,
    channel_type: &str,
    address: &str,
    trace_id: &str,
) -> Message {
    let mut attributes = serde_json::Map::new();
    attributes.insert("channel_type".to_string(), json!(channel_type));
    attributes.insert("address".to_string(), json!(address));

    let payload = FrontdeskHandoffPayload {
        payload_type: FRONTDESK_HANDOFF_TYPE.to_string(),
        schema_version: 1,
        operation: FRONTDESK_OPERATION_REGISTER.to_string(),
        subject: FrontdeskHandoffSubject {
            display_name: None,
            email: None,
            phone: None,
            company_name: None,
            attributes: Some(attributes),
        },
        tenant_id: tenant_id.map(ToString::to_string),
        context: None,
    };

    Message {
        routing: Routing {
            src: node_uuid.to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(cfg.target.clone()),
            ttl: 16,
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            msg_type: "data".to_string(),
            msg: Some(FRONTDESK_HANDOFF_TYPE.to_string()),
            src_ilk: Some(src_ilk.to_string()),
            ..Meta::default()
        },
        payload: serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
    }
}

/// Pasa al emisor por el frontdesk si corresponde, y despues deja seguir el mensaje original.
///
/// Devuelve siempre `()`: es una COMPUERTA QUE NO FRENA. Si el handoff falla —frontdesk caido, sin
/// ruta— se loguea a `error` y el mensaje del usuario **se entrega igual**.
///
/// Esa eleccion es deliberada y es la unica de este modulo que no es puramente tecnica: tirar el
/// mensaje de un cliente porque un nodo interno no contesto es peor que entregarlo sin registrar.
/// El precio es que el alta puede perderse en silencio salvo por ese log. Si se prefiere lo
/// contrario (fail-closed: no entregar a desconocidos), es un cambio de una linea aca — pero es
/// decision del operador, no del codigo.
pub async fn gate_then_forward(
    sender: &fluxbee_sdk::NodeSender,
    cfg: &FrontdeskGateConfig,
    hive_id: &str,
    channel_type: &str,
    address: &str,
    tenant_id: Option<&str>,
    src_ilk: Option<&str>,
    trace_id: &str,
) {
    if !cfg.enabled {
        return;
    }
    let Some(src_ilk) = src_ilk.map(str::trim).filter(|v| !v.is_empty()) else {
        // Sin ILK no hay sujeto que presentar. No es un error: es un emisor que ni siquiera pudo
        // provisionarse, y eso ya se loguea en el camino de identidad.
        return;
    };
    let status = sender_registration_status(
        hive_id,
        channel_type,
        address,
        tenant_id.unwrap_or(""),
    );
    if !gate_required(status.as_deref()) {
        return;
    }

    let msg = build_handoff_message(
        sender.uuid(),
        cfg,
        src_ilk,
        tenant_id,
        channel_type,
        address,
        trace_id,
    );
    tracing::info!(
        target_node = %cfg.target,
        src_ilk = %src_ilk,
        registration_status = %status.as_deref().unwrap_or("?"),
        %trace_id,
        "primer contacto: presentando el emisor al frontdesk antes de entregar"
    );
    if let Err(err) = sender.send(msg).await {
        tracing::error!(
            error = ?err,
            target_node = %cfg.target,
            src_ilk = %src_ilk,
            %trace_id,
            "no se pudo presentar el emisor al frontdesk; el mensaje se entrega igual y el alta \
             queda pendiente"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_compuerta_viene_apagada() {
        let cfg = FrontdeskGateConfig::default();
        assert!(!cfg.enabled, "encenderla cambia un hive vivo: default = apagada");
        assert_eq!(cfg.target, DEFAULT_FRONTDESK_TARGET);

        // Config sin la seccion => apagada.
        let vacia = FrontdeskGateConfig::from_effective_config(Some(&json!({"io":{}})));
        assert!(!vacia.enabled);
        // Y sin config alguna, tambien.
        assert!(!FrontdeskGateConfig::from_effective_config(None).enabled);
    }

    #[test]
    fn se_enciende_solo_por_config_explicita() {
        let cfg = FrontdeskGateConfig::from_effective_config(Some(&json!({
            "io": {"frontdesk": {"enabled": true, "target": "SY.frontdesk.gov@motherbee"}}
        })));
        assert!(cfg.enabled);
        assert_eq!(cfg.target, "SY.frontdesk.gov@motherbee");
    }

    #[test]
    fn un_target_vacio_no_manda_el_primer_contacto_a_la_nada() {
        let cfg = FrontdeskGateConfig::from_effective_config(Some(&json!({
            "io": {"frontdesk": {"enabled": true, "target": "   "}}
        })));
        assert!(cfg.enabled);
        assert_eq!(
            cfg.target, DEFAULT_FRONTDESK_TARGET,
            "un target en blanco cae al default, no a una cadena vacia"
        );
    }

    #[test]
    fn solo_pasa_por_el_frontdesk_quien_no_esta_completo() {
        assert!(gate_required(Some("temporary")));
        assert!(gate_required(Some("partial")));
        assert!(!gate_required(Some("complete")));
        assert!(!gate_required(Some("COMPLETE")), "el status es case-insensitive");

        // No saber NO frena el canal.
        assert!(!gate_required(None));
        assert!(!gate_required(Some("")));
        assert!(!gate_required(Some("   ")));
    }

    #[test]
    fn el_handoff_presenta_al_sujeto_aunque_solo_haya_un_user_id() {
        let cfg = FrontdeskGateConfig::default();
        let msg = build_handoff_message(
            "uuid-nodo",
            &cfg,
            "ilk:abc",
            Some("tnt:1111"),
            "slack",
            "IO.slack.test@motherbee:U0HUMANO",
            "trace-1",
        );

        match &msg.routing.dst {
            Destination::Unicast(dst) => assert_eq!(dst, &cfg.target),
            other => panic!("el handoff va unicast al frontdesk, no {other:?}"),
        }
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:abc"));
        assert_eq!(msg.meta.msg.as_deref(), Some(FRONTDESK_HANDOFF_TYPE));

        // Es el camino ESTRUCTURADO, no el conversacional: tiene que parsear como handoff.
        let parsed = crate::frontdesk_contract::parse_frontdesk_handoff_payload(&msg.payload)
            .expect("el frontdesk tiene que poder parsearlo como handoff estructurado");
        assert_eq!(parsed.operation, FRONTDESK_OPERATION_REGISTER);
        assert_eq!(parsed.tenant_id.as_deref(), Some("tnt:1111"));
        let attrs = parsed.subject.attributes.expect("attributes");
        assert_eq!(attrs.get("channel_type").and_then(Value::as_str), Some("slack"));
        assert!(attrs
            .get("address")
            .and_then(Value::as_str)
            .is_some_and(|a| a.contains("U0HUMANO")));
    }
}
