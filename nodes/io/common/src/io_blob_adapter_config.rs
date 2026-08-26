//! Contrato de configuracion de `IO.blob`.
//!
//! # Por que existe
//!
//! io.blob era, como io.cloud, un nodo systemd empaquetado que se configuraba por variables de entorno
//! y no participaba del plano de control. La regla del operador es: **todo lo que se setea en un nodo
//! va por CONFIG_SET/GET, y nada por ENV**. Esto es esa regla aplicada a io.blob, para que sea un
//! ciudadano de primera como los demas nodos IO (io.api/io.slack/io.wapp).
//!
//! # Que es config y que no
//!
//! io.blob es un CURADOR: recibe comandos ya autorizados de `SY.admin`, cura bytes a `public/` y
//! mantiene un ledger/refcount local. La mayoria de lo que hoy sale del entorno NO es config de
//! operador: los roots (`blob_root`, `public_root`, `ledger_path`) son **rutas fijas de diseño del
//! sistema** (defaults del nodo, con los dirs creados al instalar), y `admin_hive` es **derivable del
//! propio hive** (por defecto el local). Lo unico que un operador podria querer tunear:
//!
//! - **`max_bytes`** — el tamaño maximo de un blob publicable (limite de politica).
//! - **`admin_hive`** — override del hive de `SY.admin` en el que confia (por defecto el local).
//!
//! **No hay campo de secreto, a proposito:** io.blob no acuña ni guarda tokens; el acceso a un
//! artefacto publico lo gobierna la fila del edge (capability en la URL), no este nodo.
//!
//! # Quien lo escribe
//!
//! Normalmente **nadie a mano**: un io.blob sin config es un curador VALIDO con defaults (no
//! `FAILED_CONFIG`). `CONFIG_SET` es el override cuando hace falta subir/bajar `max_bytes` o apuntar
//! a otro `admin_hive`.

use serde_json::{json, Value};

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};

pub struct IoBlobAdapterConfigContract;

/// Override del hive de `SY.admin` (por defecto el local). Rara vez se toca.
pub const FIELD_ADMIN_HIVE: &str = "config.io.admin_hive";
/// Tamaño maximo de un blob publicable, en bytes.
pub const FIELD_MAX_BYTES: &str = "config.io.max_bytes";

impl IoAdapterConfigContract for IoBlobAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.blob"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // Ninguno. Un io.blob sin config es un curador VALIDO con defaults (roots de diseño,
        // admin_hive local, max_bytes por defecto). Exigir algo lo dejaria en FAILED_CONFIG en cada
        // arranque limpio sin aportar nada.
        &[]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[FIELD_ADMIN_HIVE, FIELD_MAX_BYTES]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "io.blob is a curator: it takes already-authorized commands from SY.admin, curates bytes \
             into public/, and keeps a local publication/refcount ledger.",
            "The blob roots and ledger path are FIXED system paths (node defaults, dirs created at \
             install), NOT operator config. admin_hive defaults to the local hive.",
            "There is deliberately NO secret field: io.blob mints and stores no tokens; public-artifact \
             access is governed by the edge allowlist row (the capability lives in the URL).",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let io = candidate.get("io");

        // `admin_hive`, si viene, tiene que ser un id de hive usable. Un valor en blanco es un typo,
        // no "sin override".
        let admin_hive = match io.and_then(|io| io.get("admin_hive")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.admin_hive must not be blank; omit the field to use the local hive"
                            .to_string(),
                    ));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.admin_hive must be a string".to_string(),
                ))
            }
        };

        // `max_bytes`, si viene, tiene que ser un entero positivo. 0 no es "sin limite": es un limite
        // que rechaza TODO, casi siempre un error. Omitir el campo = usar el default del nodo.
        let max_bytes = match io.and_then(|io| io.get("max_bytes")) {
            None | Some(Value::Null) => None,
            Some(value) => {
                let n = value.as_u64().ok_or_else(|| {
                    IoAdapterConfigError::InvalidConfig(
                        "io.max_bytes must be a non-negative integer".to_string(),
                    )
                })?;
                if n == 0 {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.max_bytes must be > 0 (0 would reject every publish); omit to use the default"
                            .to_string(),
                    ));
                }
                Some(n)
            }
        };

        let mut io_out = serde_json::Map::new();
        if let Some(admin_hive) = admin_hive {
            io_out.insert("admin_hive".to_string(), json!(admin_hive));
        }
        if let Some(max_bytes) = max_bytes {
            io_out.insert("max_bytes".to_string(), json!(max_bytes));
        }
        Ok(json!({ "io": Value::Object(io_out) }))
    }
}

/// El hive de `SY.admin` en el que confia, leido de la config efectiva. `None` = usar el local.
pub fn configured_admin_hive(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("admin_hive"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// El tope de tamaño de blob configurado, leido de la config efectiva. `None` = usar el default.
pub fn configured_max_bytes(effective: Option<&Value>) -> Option<u64> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("max_bytes"))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_curador_sin_config_es_valido_no_un_error() {
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({}))
            .expect("un io.blob sin config es valido");
        assert!(configured_admin_hive(Some(&out)).is_none());
        assert!(configured_max_bytes(Some(&out)).is_none());
        assert!(IoBlobAdapterConfigContract.required_fields().is_empty());
    }

    #[test]
    fn acepta_max_bytes_y_admin_hive_y_los_expone() {
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"max_bytes": 1048576, "admin_hive": " motherbee "}}))
            .expect("config valida");
        assert_eq!(configured_max_bytes(Some(&out)), Some(1_048_576));
        assert_eq!(
            configured_admin_hive(Some(&out)).as_deref(),
            Some("motherbee"),
            "se normaliza el espaciado"
        );
    }

    #[test]
    fn max_bytes_cero_es_un_error_no_un_silencio() {
        let err = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"max_bytes": 0}}))
            .expect_err("un tope 0 rechazaria TODO publish");
        assert!(format!("{err}").contains("> 0"), "{err}");
    }

    #[test]
    fn admin_hive_en_blanco_es_un_error() {
        let err = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"admin_hive": "   "}}))
            .expect_err("un admin_hive en blanco tiene que fallar");
        assert!(format!("{err}").contains("blank"), "{err}");
    }

    /// El contrato NO acepta secretos: io.blob no acuña ni guarda tokens.
    #[test]
    fn no_hay_campo_de_secreto_en_el_contrato() {
        let campos: Vec<&str> = IoBlobAdapterConfigContract
            .optional_fields()
            .iter()
            .chain(IoBlobAdapterConfigContract.required_fields())
            .copied()
            .collect();
        for campo in &campos {
            let bajo = campo.to_ascii_lowercase();
            assert!(
                !bajo.contains("secret") && !bajo.contains("token") && !bajo.contains("api_key"),
                "el contrato de io.blob no debe tener campos de secreto: {campo}"
            );
        }
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"max_bytes": 1024, "secret": "x"}}))
            .expect("valido");
        assert!(
            out.get("io").and_then(|io| io.get("secret")).is_none(),
            "un secreto colado no puede sobrevivir a la materializacion"
        );
    }
}
