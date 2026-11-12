package node

import (
	"fmt"
	"path/filepath"
	"strings"
)

// Engine is the wf.engine a WF node runs (FINDINGS A-302): the runtime and the version whose
// directory holds its binary, as the orchestrator launches it
// (<dist>/runtimes/<runtime>/<version>/bin/<binary>, the start script beside it its unit's
// ExecStart), and the binary's path. The node's runtime, as get_node_status shows it, is its
// workflow package (wf.<name>) on the package's own version: until 0.1.139 only the unit's
// ExecStart said which engine ran it.
type Engine struct {
	Runtime    string
	Version    string
	Executable string
}

// EngineOf is the engine whose binary is at executable: its runtime and version when the path is a
// runtime version's, <...>/runtimes/<runtime>/<version>/bin/<binary>, and only its path otherwise
// (a binary run by hand, or a test's).
func EngineOf(executable string) Engine {
	engine := Engine{Executable: executable}
	if executable == "" {
		return engine
	}
	parts := strings.Split(filepath.ToSlash(filepath.Clean(executable)), "/")
	if n := len(parts); n >= 5 && parts[n-5] == "runtimes" && parts[n-2] == "bin" && parts[n-4] != "" && parts[n-3] != "" {
		engine.Runtime, engine.Version = parts[n-4], parts[n-3]
	}
	return engine
}

// String is the engine as the node's log names it at its start: "wf.engine 0.1.139 (<path>)".
func (e Engine) String() string {
	switch {
	case e.Version != "":
		return fmt.Sprintf("%s %s (%s)", e.Runtime, e.Version, e.Executable)
	case e.Executable != "":
		return fmt.Sprintf("of no runtime version (%s)", e.Executable)
	}
	return "unknown: its binary's path could not be read"
}

// status is the engine as NODE_STATUS_GET reports it, its extensions.engine: runtime and version,
// null when the binary's path names none, and executable, the path ("" when the node could not
// read it).
func (e Engine) status() map[string]any {
	status := map[string]any{"runtime": nil, "version": nil, "executable": e.Executable}
	if e.Version != "" {
		status["runtime"], status["version"] = e.Runtime, e.Version
	}
	return status
}
