package wfcel

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Every workflow definition the repository ships validates: SY.admin's help example (this
// testdata/admin_help_definition.json, which a test of SY.admin compares with its help), the example
// packages and Archi's cookbook seeds. They taught meta.type, a timer key at the wrong path and a
// JSON-Schema default the engine never applies (FINDINGS A-117); the help example did not even
// validate.
func TestEveryShippedWorkflowDefinitionValidates(t *testing.T) {
	examples, err := filepath.Glob(filepath.Join("..", "..", "nodes", "wf", "examples", "packages", "*", "flow", "definition.json"))
	if err != nil || len(examples) < 2 {
		t.Fatalf("example packages: %v %v", examples, err)
	}
	for _, path := range append([]string{filepath.Join("testdata", "admin_help_definition.json")}, examples...) {
		if _, err := LoadDefinitionFile(path, fixedClock); err != nil {
			t.Errorf("%s: %v", path, err)
		}
	}

	seeds, err := filepath.Glob(filepath.Join("..", "..", "..", "seeds", "cookbook", "*.json"))
	if err != nil || len(seeds) == 0 {
		t.Fatalf("cookbook seeds: %v %v", seeds, err)
	}
	found := 0
	for _, path := range seeds {
		raw, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		var doc any
		if err := json.Unmarshal(raw, &doc); err != nil {
			t.Fatalf("%s: %v", path, err)
		}
		for _, definition := range workflowDefinitionsIn(doc) {
			found++
			data, _ := json.Marshal(definition)
			if _, err := LoadDefinitionBytes(data, path, fixedClock); err != nil {
				t.Errorf("%v", err)
			}
		}
	}
	if found < 2 {
		t.Fatalf("expected the design and artifact seeds' workflow definitions, found %d", found)
	}
}

// Every workflow definition the docs show validates too: each ```json block that holds a
// wf_schema_version is JSON, and the definitions in it load. The spec's case study and the
// quickstart's example taught an agent's *_RESPONSE (an agent answers with the request's own msg),
// a wait for a customer's answer no instance gets, and an optional field without a default (0.1.90
// review). A sketch with "..." is fenced as plain text, not json.
func TestEveryWorkflowDefinitionInTheDocsValidates(t *testing.T) {
	root := filepath.Join("..", "..", "..")
	found := 0
	for _, doc := range []string{
		"docs/spec-systemnode-wf-v1_1.md",
		"docs/wf-rules-quickstart.md",
		"docs/sy-wf-rules-spec.md",
		"docs/nodes/sy-wf-rules.md",
		"docs/archi/handbook_fluxbee.md",
		"go/nodes/wf/README.md",
		"go/nodes/wf/wf-generic/README.md",
	} {
		raw, err := os.ReadFile(filepath.Join(root, doc))
		if err != nil {
			t.Fatal(err)
		}
		for _, block := range jsonBlocks(string(raw)) {
			if !strings.Contains(block.text, `"wf_schema_version"`) {
				continue
			}
			var parsed any
			if err := json.Unmarshal([]byte(block.text), &parsed); err != nil {
				t.Errorf("%s:%d: a json block that is no JSON (fence a sketch as plain text): %v", doc, block.line, err)
				continue
			}
			for _, definition := range workflowDefinitionsIn(parsed) {
				found++
				data, _ := json.Marshal(definition)
				if _, err := LoadDefinitionBytes(data, "", fixedClock); err != nil {
					t.Errorf("%s:%d: %v", doc, block.line, err)
				}
			}
		}
	}
	if found < 2 {
		t.Fatalf("expected the spec's and the quickstart's workflow definitions, found %d", found)
	}
}

// Archi's handbook lists the known options (docs/archi/handbook_fluxbee.md §4.2.4) as KnownOptions
// declares them: the table between its known-options marks is the catalog rendered, so the list Archi
// writes from never drifts from the one the load writes in (wf-orchestration-v1 §5.4.8).
func TestTheHandbookListsTheKnownOptions(t *testing.T) {
	const path, start, end = "docs/archi/handbook_fluxbee.md", "<!-- known-options:start -->", "<!-- known-options:end -->"
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", path))
	if err != nil {
		t.Fatal(err)
	}
	handbook := string(raw)
	if strings.Count(handbook, start) != 1 || strings.Count(handbook, end) != 1 {
		t.Fatalf("%s holds the marks %s and %s once each", path, start, end)
	}
	_, rest, _ := strings.Cut(handbook, start)
	listed, _, _ := strings.Cut(rest, end)
	if got, want := strings.TrimSpace(listed), knownOptionsTable(); got != want {
		t.Fatalf("%s lists other known options than KnownOptions; the table, rendered from it:\n%s\n\nthe handbook's:\n%s", path, want, got)
	}
}

// knownOptionsTable renders KnownOptions as a markdown table, in the catalog's order: each option's
// name, description and params, those in name order, each with its type (and its values, for an
// enum) and its description.
func knownOptionsTable() string {
	rows := []string{"| Name | Description | Params |", "|---|---|---|"}
	for _, option := range KnownOptions {
		var params []string
		for _, name := range sortedKeys(option.Params) {
			param := option.Params[name]
			kind := param.Type
			if len(param.Enum) > 0 {
				kind += ": " + strings.Join(param.Enum, ", ")
			}
			text := fmt.Sprintf("`%s` (%s)", name, kind)
			if param.Description != "" {
				text += ": " + param.Description
			}
			params = append(params, text)
		}
		row := fmt.Sprintf("| `%s` | %s |", option.Name, option.Description)
		if len(params) > 0 {
			row += " " + strings.Join(params, "; ")
		}
		rows = append(rows, row+" |")
	}
	return strings.Join(rows, "\n")
}

type fencedBlock struct {
	line int // of its opening fence
	text string
}

// jsonBlocks are markdown's ```json fenced blocks.
func jsonBlocks(markdown string) []fencedBlock {
	var blocks []fencedBlock
	var current *fencedBlock
	var body []string
	for i, line := range strings.Split(markdown, "\n") {
		trimmed := strings.TrimSpace(line)
		switch {
		case current == nil && trimmed == "```json":
			current = &fencedBlock{line: i + 1}
			body = nil
		case current != nil && trimmed == "```":
			current.text = strings.Join(body, "\n")
			blocks = append(blocks, *current)
			current = nil
		case current != nil:
			body = append(body, line)
		}
	}
	return blocks
}

// workflowDefinitionsIn finds every object with a wf_schema_version and states in doc (SY.wf-rules'
// metadata has a wf_schema_version too).
func workflowDefinitionsIn(doc any) []map[string]any {
	var out []map[string]any
	switch typed := doc.(type) {
	case map[string]any:
		_, versioned := typed["wf_schema_version"]
		if _, ok := typed["states"]; ok && versioned {
			return []map[string]any{typed}
		}
		for _, child := range typed {
			out = append(out, workflowDefinitionsIn(child)...)
		}
	case []any:
		for _, child := range typed {
			out = append(out, workflowDefinitionsIn(child)...)
		}
	}
	return out
}
