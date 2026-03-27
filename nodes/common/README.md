# Nodes Common

This directory documents cross-family conventions for non-`SY` nodes.

Current scope:
- common control-plane contract for runtime/business configuration
- shared metadata expectations for `CONFIG_GET` / `CONFIG_SET`
- response convention `CONFIG_RESPONSE`
- shared code helpers in `crates/fluxbee_sdk/src/node_config.rs`

Reference:
- [Node Config Control Plane Spec](/Users/cagostino/Documents/GitHub/fluxbee/docs/node-config-control-plane-spec.md)

Available shared code:
- `build_node_config_get_message(...)`
- `build_node_config_set_message(...)`
- `build_node_config_response_message(...)`
- `build_node_config_response_message_runtime_src(...)`
- `parse_node_config_request(...)`
- `parse_node_config_response(...)`

The shared code standardizes:
- common L2/system metadata
- `trace_id` reply behavior
- common payload fields for `CONFIG_GET` / `CONFIG_SET`

The shared code does not standardize:
- node-specific `payload.config`
- node-specific contract contents returned by `CONFIG_GET`
- business validation of one node family

Important:
- this is a documentation home first
- it is not yet a shared crate
- node-specific `payload.config` schemas remain owned by each node family/implementation
