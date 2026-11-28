package node

import (
	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// identityView is what a capability's door reads of its hive's identity memory
// (docs/digital-executive-v1-stage-3.md §4, §5): the ILK a node handles, with what identity says of
// it, and what identity says of an ILK. The engine reads the identity SHM through the Go SDK
// (shmIdentity); a test gives its own. Shared state stays in shared memory: the door never asks
// SY.identity.
type identityView interface {
	// ilkByNode is the active ILK whose handler_node is nodeName ("ilk:<uuid>") and its facts, false
	// when none is.
	ilkByNode(nodeName string) (string, ilkFacts, bool, error)
	// ilk is what identity says of the active ILK ilk, false when it has no such ILK.
	ilk(ilk string) (ilkFacts, bool, error)
}

// ilkFacts is what the door reads of an ILK: its type, its tenant, and the person it works for in
// what role (sdk.IlkFacts).
type ilkFacts = sdk.IlkFacts

// workRoleConscious is the role, as sdk.IlkFacts.WorkRole names it, of an ED's conscious: the only
// sender a door takes a request from. The Go SDK exports no constant for the names.
const workRoleConscious = "conscious"

// shmIdentity reads the identity SHM of hive with the Go SDK.
type shmIdentity struct {
	hive string
}

func (s shmIdentity) ilkByNode(nodeName string) (string, ilkFacts, bool, error) {
	return sdk.LookupIlkByNode(s.hive, nodeName)
}

func (s shmIdentity) ilk(ilk string) (ilkFacts, bool, error) {
	return sdk.LookupIlk(s.hive, ilk)
}
