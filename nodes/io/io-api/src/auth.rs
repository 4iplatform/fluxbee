use anyhow::Result;
use axum::http::header::AUTHORIZATION;
use fluxbee_sdk::{
    build_node_secret_record, load_node_secret_record, load_node_secret_record_with_root,
    save_node_secret_record, save_node_secret_record_with_root, NodeSecretError, NodeSecretRecord,
    NodeSecretWriteOptions,
};
use io_common::io_api_adapter_config::api_key_storage_key;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{ApiAuthRegistry, ApiKeyRuntime, AuthMatch};

pub(crate) fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let trimmed = token.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) async fn authenticate_bearer(
    registry: &Arc<RwLock<ApiAuthRegistry>>,
    token: &str,
) -> Option<AuthMatch> {
    let registry = registry.read().await;
    registry.keys.iter().find_map(|entry| {
        (entry.token == token).then(|| AuthMatch {
            key_id: entry.key_id.clone(),
            tenant_id: entry.tenant_id.clone(),
            integration_id: entry.integration_id.clone(),
            webhook_enabled: registry
                .integrations
                .get(&entry.integration_id)
                .and_then(|integration| integration.webhook.as_ref())
                .is_some(),
            caller_identity: entry.caller_identity.clone(),
        })
    })
}

pub(crate) fn prepare_runtime_api_config(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<(Value, ApiAuthRegistry)> {
    let sanitized = materialize_inline_api_keys(node_name, effective, secret_root)?;
    let registry = load_runtime_api_registry(node_name, &sanitized, secret_root)?;
    Ok((sanitized, registry))
}

pub(crate) fn materialize_inline_api_keys(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<Value> {
    let mut sanitized = effective.clone();
    let api_keys = sanitized
        .get_mut("auth")
        .and_then(Value::as_object_mut)
        .and_then(|auth| auth.get_mut("api_keys"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;

    let mut secret_record = load_or_default_secret_record(node_name, secret_root)?;
    let mut secrets_changed = false;

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object_mut() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let inline_token = entry_obj
            .get("token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let Some(token) = inline_token else {
            continue;
        };

        let storage_key = api_key_storage_key(key_id);
        secret_record
            .secrets
            .insert(storage_key.clone(), Value::String(token));
        entry_obj.remove("token");
        entry_obj.insert(
            "token_ref".to_string(),
            Value::String(format!("local_file:{storage_key}")),
        );
        secrets_changed = true;
    }

    if secrets_changed {
        let record =
            build_node_secret_record(secret_record.secrets, &NodeSecretWriteOptions::default());
        save_secret_record(node_name, secret_root, &record)?;
    }

    Ok(sanitized)
}

pub(crate) fn load_runtime_api_registry(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<ApiAuthRegistry> {
    let api_keys = effective
        .get("auth")
        .and_then(|auth| auth.get("api_keys"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;
    let mut registry = ApiAuthRegistry::default();
    let mut secret_record: Option<NodeSecretRecord> = None;

    if let Some(integrations) = effective.get("integrations").and_then(Value::as_array) {
        for entry in integrations {
            let Some(entry_obj) = entry.as_object() else {
                return Err(anyhow::anyhow!("config.integrations[] entries must be objects"));
            };
            let integration_id = entry_obj
                .get("integration_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("config.integrations[].integration_id is required"))?;
            let tenant_id = entry_obj
                .get("tenant_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("config.integrations[].tenant_id is required"))?;
            let final_reply_required = entry_obj
                .get("final_reply_required")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow::anyhow!(
                    "config.integrations[].final_reply_required is required"
                ))?;
            let webhook = entry_obj
                .get("webhook")
                .and_then(Value::as_object)
                .and_then(|webhook| {
                    if !webhook.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
                        return None;
                    }
                    Some(crate::WebhookRuntime {
                        url: webhook.get("url")?.as_str()?.to_string(),
                        secret_ref: webhook.get("secret_ref")?.as_str()?.to_string(),
                        timeout_ms: webhook.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5000),
                        max_retries: webhook.get("max_retries").and_then(Value::as_u64).unwrap_or(5),
                        initial_backoff_ms: webhook
                            .get("initial_backoff_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or(1000),
                        max_backoff_ms: webhook
                            .get("max_backoff_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or(30000),
                    })
                });
            registry.integrations.insert(
                integration_id.to_string(),
                crate::IntegrationRuntime {
                    integration_id: integration_id.to_string(),
                    tenant_id: tenant_id.to_string(),
                    final_reply_required,
                    webhook,
                },
            );
        }
    }

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let tenant_id = entry_obj
            .get("tenant_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].tenant_id is required"))?;
        let integration_id = entry_obj
            .get("integration_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].integration_id is required"))?;
        let token = resolve_api_key_token(node_name, entry_obj, &mut secret_record, secret_root)?;
        registry.keys.push(ApiKeyRuntime {
            key_id: key_id.to_string(),
            tenant_id: tenant_id.to_string(),
            integration_id: integration_id.to_string(),
            token,
            caller_identity: entry_obj.get("caller_identity").cloned(),
        });
    }

    Ok(registry)
}

fn resolve_api_key_token(
    node_name: &str,
    entry: &Map<String, Value>,
    secret_record: &mut Option<NodeSecretRecord>,
    secret_root: Option<&Path>,
) -> Result<String> {
    if let Some(token) = entry
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(token.to_string());
    }

    let reference = entry
        .get("token_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[] requires token or token_ref"))?;

    if let Some(var) = reference.strip_prefix("env:") {
        return env(var)
            .ok_or_else(|| anyhow::anyhow!("unresolved env secret reference '{reference}'"));
    }

    if let Some(storage_key) = reference.strip_prefix("local_file:") {
        if secret_record.is_none() {
            *secret_record = Some(load_or_default_secret_record(node_name, secret_root)?);
        }
        return secret_record
            .as_ref()
            .and_then(|record| record.secrets.get(storage_key))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing local secret '{storage_key}'"));
    }

    Err(anyhow::anyhow!(
        "unsupported secret reference '{reference}' for config.auth.api_keys[].token_ref"
    ))
}

pub(crate) fn resolve_secret_reference(
    node_name: &str,
    reference: &str,
    secret_root: Option<&Path>,
) -> Result<String> {
    let reference = reference.trim();
    if let Some(var) = reference.strip_prefix("env:") {
        return env(var)
            .ok_or_else(|| anyhow::anyhow!("unresolved env secret reference '{reference}'"));
    }
    if let Some(storage_key) = reference.strip_prefix("local_file:") {
        let record = load_or_default_secret_record(node_name, secret_root)?;
        return record
            .secrets
            .get(storage_key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing local secret '{storage_key}'"));
    }
    Err(anyhow::anyhow!(
        "unsupported secret reference '{reference}'"
    ))
}

fn load_or_default_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord> {
    match load_secret_record(node_name, secret_root) {
        Ok(record) => Ok(record),
        Err(NodeSecretError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(NodeSecretRecord::default())
        }
        Err(err) => Err(err.into()),
    }
}

fn load_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord, NodeSecretError> {
    match secret_root {
        Some(root) => load_node_secret_record_with_root(node_name, root),
        None => load_node_secret_record(node_name),
    }
}

fn save_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
    record: &NodeSecretRecord,
) -> Result<()> {
    match secret_root {
        Some(root) => {
            save_node_secret_record_with_root(node_name, root, record)?;
        }
        None => {
            save_node_secret_record(node_name, record)?;
        }
    }
    Ok(())
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}
