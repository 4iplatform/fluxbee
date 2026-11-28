package node

import (
	"encoding/binary"
	"fmt"
	"os"
	"runtime"
	"strings"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
	"github.com/google/uuid"
)

// identityView is what a capability's door reads of its hive's identity memory
// (docs/digital-executive-v1-stage-3.md §5): the ILK a node handles, and what identity says of an
// ILK. The engine reads the identity SHM (shmIdentity); a test gives its own. Shared state stays in
// shared memory: the door never asks SY.identity.
type identityView interface {
	// nodeILK is the active ILK whose handler_node is nodeName ("ilk:<uuid>"), false when none is.
	nodeILK(nodeName string) (string, bool, error)
	// ilk is what identity says of the active ILK ilk, false when it has no such ILK.
	ilk(ilk string) (ilkFacts, bool, error)
}

// ilkFacts is what the door reads of an ILK: its type (human, agent or system), its tenant
// ("tnt:<uuid>"), and the person it works for ("ilk:<uuid>", "" for nobody) in what role
// (conscious, channel, agent; "" for none, and, until the Go SDK reads them, for connector and
// capability).
type ilkFacts struct {
	Type     string
	TenantID string
	WorksFor string
	WorkRole string
}

// shmIdentity reads the identity SHM of hive. Whom an ILK works for, and in what role, come from the
// Go SDK (sdk.LookupIlk, since 0.1.157); its tenant from ilkTenantAt, which the SDK does not read
// yet: it stands in for the SDK's helper of stage 3's release 3A, and goes once that helper gives the
// tenant.
type shmIdentity struct {
	hive string
}

func (s shmIdentity) nodeILK(nodeName string) (string, bool, error) {
	return sdk.LookupIlkByHandlerNode(s.hive, nodeName)
}

func (s shmIdentity) ilk(ilk string) (ilkFacts, bool, error) {
	facts, found, err := sdk.LookupIlk(s.hive, ilk)
	if err != nil || !found {
		return ilkFacts{}, false, err
	}
	tenant, found, err := ilkTenantAt(identityShmPath(s.hive), ilk)
	if err != nil || !found {
		return ilkFacts{}, false, err
	}
	return ilkFacts{Type: facts.Type, TenantID: tenant, WorksFor: facts.WorksFor, WorkRole: facts.WorkRole}, true, nil
}

// identityShmPath is the identity SHM of hive, as the Go SDK names it.
func identityShmPath(hive string) string {
	return "/dev/shm/jsr-identity-" + strings.TrimSpace(hive)
}

// The identity SHM's layout an ILK's tenant is read with (src/shm/mod.rs, IdentityHeader and
// IlkEntry, version 7; the Go SDK's identity_shm.go reads the same region): the header's fields, the
// tenants' region before the ILKs', and an ILK entry's flags and tenant, which comes right before
// its display name (testdata/identity_layout/ilk_entry.json in the Go SDK).
const (
	shmIdentityMagic        uint32 = 0x4A534944 // "JSID"
	shmIdentityVersion      uint32 = 7
	shmIdentityHeaderSize          = 176
	shmIdentityRegionAlign         = 64
	shmTenantEntrySize             = 320
	shmIlkEntrySize                = 1184
	shmHeaderSeq                   = 8
	shmHeaderVersion               = 4
	shmHeaderIlkCount              = 20
	shmHeaderMaxIlks               = 40
	shmHeaderMaxTenants            = 44
	shmIlkOffFlags                 = 18
	shmIlkOffTenantID              = 20
	shmIlkFlagActive        uint16 = 0x0001
	shmIdentitySeqlockLimit        = 50 * time.Millisecond
)

// ilkTenantAt is the tenant ("tnt:<uuid>") of the active ILK ilk in the identity SHM at path, read
// under the region's seqlock as the Go SDK reads it: false when the region is absent, of another
// version or still moving after shmIdentitySeqlockLimit, and when it has no such active ILK.
func ilkTenantAt(path, ilk string) (string, bool, error) {
	wanted, err := uuid.Parse(strings.TrimPrefix(strings.TrimSpace(ilk), "ilk:"))
	if err != nil {
		return "", false, nil
	}
	file, err := os.Open(path)
	if os.IsNotExist(err) {
		return "", false, nil
	}
	if err != nil {
		return "", false, fmt.Errorf("identity shm open %q: %w", path, err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", false, fmt.Errorf("identity shm stat: %w", err)
	}
	size := int(info.Size())
	if size < shmIdentityHeaderSize {
		return "", false, nil
	}
	data, err := syscall.Mmap(int(file.Fd()), 0, size, syscall.PROT_READ, syscall.MAP_SHARED)
	if err != nil {
		return "", false, fmt.Errorf("identity shm mmap: %w", err)
	}
	defer syscall.Munmap(data) //nolint:errcheck
	if binary.LittleEndian.Uint32(data) != shmIdentityMagic || binary.LittleEndian.Uint32(data[shmHeaderVersion:]) != shmIdentityVersion {
		return "", false, nil
	}
	count := int(binary.LittleEndian.Uint32(data[shmHeaderIlkCount:]))
	maxIlks := int(binary.LittleEndian.Uint32(data[shmHeaderMaxIlks:]))
	maxTenants := int(binary.LittleEndian.Uint32(data[shmHeaderMaxTenants:]))
	if maxIlks <= 0 {
		return "", false, nil
	}
	count = min(count, maxIlks)
	offset := alignUp(alignUp(shmIdentityHeaderSize, shmIdentityRegionAlign)+shmTenantEntrySize*maxTenants, shmIdentityRegionAlign)
	if offset+count*shmIlkEntrySize > size {
		return "", false, nil
	}
	entries := data[offset : offset+count*shmIlkEntrySize]
	seq := (*uint64)(unsafe.Pointer(&data[shmHeaderSeq]))
	deadline := time.Now().Add(shmIdentitySeqlockLimit)
	for {
		before := atomic.LoadUint64(seq)
		if before&1 == 0 {
			tenant, found := scanIlkTenant(entries, count, wanted)
			if atomic.LoadUint64(seq) == before {
				return tenant, found, nil
			}
		}
		if time.Now().After(deadline) {
			return "", false, nil
		}
		runtime.Gosched()
	}
}

// scanIlkTenant is the tenant of the active entry of id among the count entries.
func scanIlkTenant(entries []byte, count int, id uuid.UUID) (string, bool) {
	for i := 0; i < count; i++ {
		entry := entries[i*shmIlkEntrySize : (i+1)*shmIlkEntrySize]
		if binary.LittleEndian.Uint16(entry[shmIlkOffFlags:])&shmIlkFlagActive == 0 || uuid.UUID(entry[:16]) != id {
			continue
		}
		tenant := uuid.UUID(entry[shmIlkOffTenantID : shmIlkOffTenantID+16])
		if tenant == (uuid.UUID{}) {
			return "", true
		}
		return "tnt:" + tenant.String(), true
	}
	return "", false
}

func alignUp(n, align int) int {
	return (n + align - 1) / align * align
}
