# WF Nodes

Sources of the `WF.*` nodes: the workflow engine and its example workflows.

- `wf-generic/`: the engine, `wf.engine`, the base runtime every `WF.*` node runs
  ([wf-generic/README.md](wf-generic/README.md)).
- `examples/packages/`: workflow packages on that engine (`wf.invoice`, `wf.router`); their
  definitions validate in CI and run on the engine in its tests.

The definition language and its validator are `go/pkg/wfcel`, shared with SY.wf-rules. The spec is
[docs/spec-systemnode-wf-v1_1.md](../../../docs/spec-systemnode-wf-v1_1.md), the deploy guide
[docs/wf-rules-quickstart.md](../../../docs/wf-rules-quickstart.md), and the track
[docs/wf-orchestration-v1.md](../../../docs/wf-orchestration-v1.md).
