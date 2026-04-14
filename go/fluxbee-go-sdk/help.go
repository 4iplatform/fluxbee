package sdk

type HelpOperationDescriptor struct {
	Verb            string         `json:"verb"`
	Description     string         `json:"description"`
	RequiredFields  []string       `json:"required_fields,omitempty"`
	OptionalFields  []string       `json:"optional_fields,omitempty"`
	Constraints     []string       `json:"constraints,omitempty"`
	ExampleRequest  map[string]any `json:"example_request,omitempty"`
	ExampleResponse map[string]any `json:"example_response,omitempty"`
}

type HelpErrorDescriptor struct {
	Code        string `json:"code"`
	Description string `json:"description"`
}

type HelpDescriptor struct {
	OK          bool                      `json:"ok"`
	Verb        string                    `json:"verb"`
	NodeFamily  string                    `json:"node_family"`
	NodeKind    string                    `json:"node_kind"`
	Version     string                    `json:"version"`
	Description string                    `json:"description"`
	Operations  []HelpOperationDescriptor `json:"operations,omitempty"`
	ErrorCodes  []HelpErrorDescriptor     `json:"error_codes,omitempty"`
}

func BuildHelpDescriptor(verb, nodeFamily, nodeKind, version, description string, operations []HelpOperationDescriptor, errorCodes []HelpErrorDescriptor) HelpDescriptor {
	return HelpDescriptor{
		OK:          true,
		Verb:        verb,
		NodeFamily:  nodeFamily,
		NodeKind:    nodeKind,
		Version:     version,
		Description: description,
		Operations:  operations,
		ErrorCodes:  errorCodes,
	}
}
