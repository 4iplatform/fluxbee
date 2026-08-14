//! Contrato de configuracion de `IO.cloud`.
//!
//! # Por que existe
//!
//! io.cloud era el unico nodo IO sin plano de control: se configuraba por variables de entorno, y
//! por eso exigia que el operador inventara un token y lo dejara en el entorno del proceso o en un
//! archivo en disco — teniendo el sistema un vault justamente para eso.
//!
//! La regla del operador es: **todo lo que se setea en un nodo va por CONFIG_SET/GET, y nada ocurre
//! sin el admin**. Esto es esa regla aplicada a io.cloud.
//!
//! # Que es config y que no
//!
//! Un solo campo es config de verdad: **`edge_node`**, el edge en el que io.cloud confia para el
//! trafico entrante. Todo lo demas de lo que hoy sale del entorno es o bien derivable del propio
//! hive (`admin_hive`, `identity_hive`), o bien identidad congelada del canal
//! (`channel_type`, `channel_address`), o bien ruta fija de diseño del sistema.
//!
//! **No hay campo de secreto, a proposito.** El token del endpoint lo acuña `SY.admin` y lo guarda
//! en el vault; ni el operador ni este nodo lo eligen ni lo ven. Aceptar un secreto aca seria
//! reabrir la puerta que se acaba de cerrar.
//!
//! # Quien lo escribe
//!
//! Normalmente **nadie a mano**: `publish_cloud_endpoint` publica el endpoint y, como parte de esa
//! misma orden, deja asentado el `edge_node` que acaba de usar. El operador puede verlo con
//! `CONFIG_GET` y cambiarlo con `CONFIG_SET` si hace falta, pero el camino feliz no se lo pide.

use serde_json::{json, Value};

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};

pub struct IoCloudAdapterConfigContract;

/// El unico campo que un operador elige.
pub const FIELD_EDGE_NODE: &str = "config.io.edge_node";

impl IoAdapterConfigContract for IoCloudAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.cloud"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // Ninguno. Un io.cloud sin `edge_node` es un nodo VALIDO que todavia no tiene puerta
        // publica: atiende la malla y no confia en ningun edge. Exigirlo lo dejaria en
        // FAILED_CONFIG en cada arranque limpio, que es exactamente el estado que confunde al
        // operador y no aporta nada.
        &[]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[FIELD_EDGE_NODE, "config.io.inbound_family"]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "edge_node is the SY.edge this node trusts for INBOUND traffic; a message from any \
             other origin is dropped.",
            "It is normally set for you by the publish_cloud_endpoint admin action, which publishes \
             the endpoint and records the edge it used. CONFIG_SET is the manual override.",
            "There is deliberately NO secret field: the endpoint token is minted by SY.admin into \
             the vault, and the edge only ever receives a reference to it.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let io = candidate.get("io");

        // `edge_node`, si viene, tiene que ser un nombre L2 usable. Un valor en blanco no es
        // "sin edge": es un error de tipeo que dejaria al nodo descartando todo en silencio.
        let edge_node = match io.and_then(|io| io.get("edge_node")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.edge_node must not be blank; omit the field to trust no edge yet"
                            .to_string(),
                    ));
                }
                if !trimmed.starts_with("SY.edge") {
                    return Err(IoAdapterConfigError::InvalidConfig(format!(
                        "io.edge_node must be an SY.edge L2 name (got '{trimmed}')"
                    )));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.edge_node must be a string".to_string(),
                ))
            }
        };

        let inbound_family = match io.and_then(|io| io.get("inbound_family")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.inbound_family must not be blank".to_string(),
                    ));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.inbound_family must be a string".to_string(),
                ))
            }
        };

        let mut io_out = serde_json::Map::new();
        if let Some(edge_node) = edge_node {
            io_out.insert("edge_node".to_string(), json!(edge_node));
        }
        if let Some(inbound_family) = inbound_family {
            io_out.insert("inbound_family".to_string(), json!(inbound_family));
        }
        Ok(json!({ "io": Value::Object(io_out) }))
    }
}

/// El edge en el que este nodo confia, leido de la config efectiva. `None` = todavia ninguno.
pub fn trusted_edge_node(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("edge_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_nodo_sin_edge_es_valido_no_un_error() {
        // Sin puerta publica todavia: atiende la malla y no confia en nadie. Exigir el campo lo
        // dejaria en FAILED_CONFIG en cada arranque limpio sin aportar nada.
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({}))
            .expect("un io.cloud sin edge es valido");
        assert!(trusted_edge_node(Some(&out)).is_none());
        assert!(IoCloudAdapterConfigContract.required_fields().is_empty());
    }

    #[test]
    fn acepta_un_edge_valido_y_lo_expone() {
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": " SY.edge@ingress1 "}}))
            .expect("edge valido");
        assert_eq!(
            trusted_edge_node(Some(&out)).as_deref(),
            Some("SY.edge@ingress1"),
            "se normaliza el espaciado"
        );
    }

    #[test]
    fn un_edge_en_blanco_es_un_error_no_un_silencio() {
        // Esto es lo que separa "todavia no tengo puerta" de "me equivoque al escribirlo". Sin este
        // corte, un typo dejaria al nodo descartando TODO el trafico entrante sin decir por que.
        let err = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": "   "}}))
            .expect_err("un edge en blanco tiene que fallar");
        assert!(format!("{err}").contains("blank"), "{err}");
    }

    #[test]
    fn el_edge_tiene_que_ser_un_sy_edge() {
        let err = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": "IO.api@motherbee"}}))
            .expect_err("solo un SY.edge puede ser la puerta");
        assert!(format!("{err}").contains("SY.edge"), "{err}");
    }

    /// El contrato NO acepta secretos. Aceptarlos reabriria la puerta que cerramos: el token lo
    /// acuña SY.admin al vault y nadie mas lo elige.
    #[test]
    fn no_hay_campo_de_secreto_en_el_contrato() {
        let campos: Vec<&str> = IoCloudAdapterConfigContract
            .optional_fields()
            .iter()
            .chain(IoCloudAdapterConfigContract.required_fields())
            .copied()
            .collect();
        for campo in &campos {
            let bajo = campo.to_ascii_lowercase();
            assert!(
                !bajo.contains("secret") && !bajo.contains("token") && !bajo.contains("api_key"),
                "el contrato de io.cloud no debe tener campos de secreto: {campo}"
            );
        }
        // Y si alguien manda uno igual, se descarta al materializar (no se propaga).
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": "SY.edge@ingress1", "secret": "x"}}))
            .expect("valido");
        assert!(
            out.get("io").and_then(|io| io.get("secret")).is_none(),
            "un secreto colado no puede sobrevivir a la materializacion"
        );
    }
}
