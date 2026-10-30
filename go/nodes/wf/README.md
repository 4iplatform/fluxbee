# WF Nodes

Sources of the `WF.*` nodes: the workflow engine and its example workflows.

- `wf-generic/`: the engine, `wf.engine`, the base runtime every `WF.*` node runs
  ([wf-generic/README.md](wf-generic/README.md)).
- `examples/packages/`: workflow packages on that engine (`wf.invoice`, `wf.router`, and
  `wf.sales.invoice`, Cases 1 and 3 of the track: a conversation handed among agents at an IO
  node's exit, each asked with a menu, with a consultation of the supervisors' IO node and a
  catch); their definitions validate in CI and run on the engine in its tests.

The definition language and its validator are `go/pkg/wfcel`, shared with SY.wf-rules. Writing a
definition is the handbook's §4.2
([docs/archi/handbook_fluxbee.md](../../../docs/archi/handbook_fluxbee.md)). The spec is
[docs/spec-systemnode-wf-v1_1.md](../../../docs/spec-systemnode-wf-v1_1.md), the deploy guide
[docs/wf-rules-quickstart.md](../../../docs/wf-rules-quickstart.md), and the track
[docs/wf-orchestration-v1.md](../../../docs/wf-orchestration-v1.md).
