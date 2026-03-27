# Nodes Common

This directory documents cross-family conventions for non-`SY` nodes.

Current scope:
- common control-plane contract for runtime/business configuration
- shared metadata expectations for `CONFIG_GET` / `CONFIG_SET`
- response convention `CONFIG_RESPONSE`

Reference:
- [Node Config Control Plane Spec](/Users/cagostino/Documents/GitHub/fluxbee/docs/node-config-control-plane-spec.md)

Important:
- this is a documentation home first
- it is not yet a shared crate
- node-specific `payload.config` schemas remain owned by each node family/implementation
