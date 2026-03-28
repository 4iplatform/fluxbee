# Nodes Common

This directory documents cross-family conventions for non-`SY` nodes.

Current scope:
- common control-plane contract for runtime/business configuration
- shared metadata expectations for `CONFIG_GET` / `CONFIG_SET`
- shared secret metadata expectations for `contract.secrets[*]`
- response convention `CONFIG_RESPONSE`
- shared code helpers in `crates/fluxbee_sdk/src/node_config.rs`
- shared secret storage helpers in `crates/fluxbee_sdk/src/node_secret.rs`

Reference:
- [Node Config Control Plane Spec](/Users/cagostino/Documents/GitHub/fluxbee/docs/node-config-control-plane-spec.md)
- [Node Secret Config Spec](/Users/cagostino/Documents/GitHub/fluxbee/docs/onworking%20COA/node-secret-config-spec.md)

Available shared code:
- `build_node_config_get_message(...)`
- `build_node_config_set_message(...)`
- `build_node_config_response_message(...)`
- `build_node_config_response_message_runtime_src(...)`
- `parse_node_config_request(...)`
- `parse_node_config_response(...)`
- `NodeSecretDescriptor`
- `build_node_secret_record(...)`
- `load_node_secret_record(...)`
- `save_node_secret_record(...)`
- `redacted_node_secret_record(...)`

The shared code standardizes:
- common L2/system metadata
- `trace_id` reply behavior
- common payload fields for `CONFIG_GET` / `CONFIG_SET`
- common local secret storage mechanics
- common metadata shape for `contract.secrets[*]`

The shared code does not standardize:
- node-specific `payload.config`
- node-specific contract contents returned by `CONFIG_GET`
- business validation of one node family

Important:
- this is a documentation home first
- it is not yet a shared crate
- node-specific `payload.config` schemas remain owned by each node family/implementation
