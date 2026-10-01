package sdk

import (
	"crypto/sha256"
	"strings"

	"github.com/google/uuid"
)

// DeterministicSystemIlkID is the ILK SY.identity seeds for a system node (Model D'): derived from
// the full L2 name, so a node knows its own ILK without waiting for the identity SHM — which does
// not exist on hives without SY.identity (ingress, egress). Byte-for-byte the Rust
// fluxbee_sdk::deterministic_system_ilk_id.
func DeterministicSystemIlkID(nodeName string) string {
	digest := sha256.Sum256([]byte("fluxbee:identity:system-ilk:v1:" + strings.TrimSpace(nodeName)))
	var raw [16]byte
	copy(raw[:], digest[:16])
	raw[6] = (raw[6] & 0x0f) | 0x50
	raw[8] = (raw[8] & 0x3f) | 0x80
	return "ilk:" + uuid.UUID(raw).String()
}
