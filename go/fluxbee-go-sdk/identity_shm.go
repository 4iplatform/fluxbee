package sdk

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"os"
	"runtime"
	"strings"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"

	"github.com/google/uuid"
)

const (
	identityMagic            uint32 = 0x4A534944 // "JSID"
	identityVersionCurrent   uint32 = 6
	identityRegionAlign             = 64
	identitySeqlockTimeoutMs        = 50

	// Struct sizes mirroring Rust #[repr(C)] layouts in src/shm/mod.rs (Identity
	// structs). The Rust file has const-asserts pinning these exact bytes; keep
	// the Go constants in lockstep or this SDK will read every entry at the
	// wrong offset and silently miss them.
	identityHeaderSize  = 176
	tenantEntrySize     = 320
	ilkEntrySize        = 1160
	ichEntrySize        = 488
	ichMappingEntrySize = 384

	// Field offsets within IchMappingEntry (384 bytes total).
	ichMapOffHash        = 0   // u64
	ichMapOffChannelType = 8   // [u8; 32]
	ichMapOffAddress     = 40  // [u8; 256]
	ichMapOffIchID       = 296 // [u8; 16]
	ichMapOffIlkID       = 312 // [u8; 16]
	ichMapOffTenantID    = 328 // [u8; 16]
	ichMapOffFlags       = 344 // u16

	// Field offsets within IlkEntry (1160 bytes total).
	ilkOffIlkID       = 0
	ilkOffType        = 16  // u8: 0 human, 1 agent, 2 system
	ilkOffFlags       = 18  // u16
	ilkOffDisplayName = 36  // [u8; 128]
	ilkOffHandlerNode = 164 // [u8; 128]

	// IlkEntry flags.
	ilkFlagActive uint16 = 0x0001

	// Field offsets within IdentityHeader.
	hdrOffMagic          = 0
	hdrOffVersion        = 4
	hdrOffSeq            = 8 // AtomicU64
	hdrOffIlkCount       = 20
	hdrOffMaxIlks        = 40
	hdrOffMaxTenants     = 44
	hdrOffMaxIchs        = 48
	hdrOffMaxIchMappings = 52

	// IchMappingEntry flags.
	ichMapFlagOccupied  uint16 = 0x01
	ichMapFlagTombstone uint16 = 0x02
)

// LookupIlkByChannel resolves an ILK ID from the identity shared memory region
// using the (hiveID, tenantID, channelType, address) key.
//
// hiveID is the raw hive identifier (e.g. "prod-hive-1") — no prefix.
// tenantID must be in "tnt:<uuid>" format.
// Returns "ilk:<uuid>" when found and (_, false, nil) when not present.
// Errors are returned only for fatal I/O or mapping problems; a missing SHM
// file is treated as not-found (false, nil).
func LookupIlkByChannel(hiveID, tenantID, channelType, address string) (string, bool, error) {
	name := fmt.Sprintf("/jsr-identity-%s", strings.TrimSpace(hiveID))
	return lookupIlkByShmName(name, tenantID, channelType, address)
}

func lookupIlkByShmName(shmName, tenantID, channelType, address string) (string, bool, error) {
	path := shmFilePath(shmName)
	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return "", false, nil
		}
		return "", false, fmt.Errorf("identity shm open %q: %w", path, err)
	}
	defer f.Close()

	fi, err := f.Stat()
	if err != nil {
		return "", false, fmt.Errorf("identity shm stat: %w", err)
	}
	size := int(fi.Size())
	if size < identityHeaderSize {
		return "", false, nil
	}

	data, err := syscall.Mmap(int(f.Fd()), 0, size, syscall.PROT_READ, syscall.MAP_SHARED)
	if err != nil {
		return "", false, fmt.Errorf("identity shm mmap: %w", err)
	}
	defer syscall.Munmap(data) //nolint:errcheck

	magic := binary.LittleEndian.Uint32(data[hdrOffMagic:])
	if magic != identityMagic {
		return "", false, nil
	}
	version := binary.LittleEndian.Uint32(data[hdrOffVersion:])
	if version != identityVersionCurrent {
		return "", false, nil
	}

	maxIlks := int(binary.LittleEndian.Uint32(data[hdrOffMaxIlks:]))
	maxTenants := int(binary.LittleEndian.Uint32(data[hdrOffMaxTenants:]))
	maxIchs := int(binary.LittleEndian.Uint32(data[hdrOffMaxIchs:]))
	maxMappings := int(binary.LittleEndian.Uint32(data[hdrOffMaxIchMappings:]))

	if maxMappings <= 0 {
		return "", false, nil
	}

	mapOffset := identityIchMappingOffset(maxTenants, maxIlks, maxIchs)
	needed := mapOffset + maxMappings*ichMappingEntrySize
	if needed > size {
		return "", false, nil
	}

	// Normalize lookup key.
	ct := strings.ToLower(strings.TrimSpace(channelType))
	addr := strings.ToLower(strings.TrimSpace(address))
	if ct == "" || addr == "" {
		return "", false, nil
	}
	tenantBytes := parseTenantUUIDBytes(tenantID)
	hash := identityIchHash(ct, addr, tenantBytes)

	// Seqlock read loop: spin while the writer holds the lock (odd seq),
	// and retry if the sequence changes mid-read.
	seqPtr := (*uint64)(unsafe.Pointer(&data[hdrOffSeq]))
	deadline := time.Now().Add(identitySeqlockTimeoutMs * time.Millisecond)
	for {
		s1 := atomic.LoadUint64(seqPtr)
		if s1&1 != 0 {
			runtime.Gosched()
			if time.Now().After(deadline) {
				return "", false, nil
			}
			continue
		}

		ilkBytes, found := identitySearchMappings(data[mapOffset:], maxMappings, hash, ct, addr, tenantBytes)

		s2 := atomic.LoadUint64(seqPtr)
		if s1 == s2 {
			if found {
				return "ilk:" + uuid.UUID(ilkBytes).String(), true, nil
			}
			return "", false, nil
		}
		if time.Now().After(deadline) {
			return "", false, nil
		}
	}
}

// identitySearchMappings performs a linear-probe lookup in the ich_mapping hash table.
func identitySearchMappings(table []byte, maxMappings int, hash uint64, channelType, address string, tenantID [16]byte) ([16]byte, bool) {
	if len(table) < maxMappings*ichMappingEntrySize {
		return [16]byte{}, false
	}
	start := int(hash % uint64(maxMappings))
	for probe := 0; probe < maxMappings; probe++ {
		pos := ((start + probe) % maxMappings) * ichMappingEntrySize
		entry := table[pos : pos+ichMappingEntrySize]

		flags := binary.LittleEndian.Uint16(entry[ichMapOffFlags:])
		if flags == 0 {
			return [16]byte{}, false // empty slot — probe chain ends here
		}
		if flags&ichMapFlagTombstone != 0 {
			continue
		}
		if flags&ichMapFlagOccupied == 0 {
			continue
		}
		entryHash := binary.LittleEndian.Uint64(entry[ichMapOffHash:])
		if entryHash != hash {
			continue
		}
		if !identityFixedStrMatches(entry[ichMapOffChannelType:ichMapOffChannelType+32], channelType) {
			continue
		}
		if !identityFixedStrMatches(entry[ichMapOffAddress:ichMapOffAddress+256], address) {
			continue
		}
		var tid [16]byte
		copy(tid[:], entry[ichMapOffTenantID:ichMapOffTenantID+16])
		if tid != tenantID {
			continue
		}
		var ilkID [16]byte
		copy(ilkID[:], entry[ichMapOffIlkID:ichMapOffIlkID+16])
		return ilkID, true
	}
	return [16]byte{}, false
}

// identityIchMappingOffset computes the byte offset of the ich_mapping table
// within the identity SHM region, mirroring layout_identity() in shm/mod.rs.
func identityIchMappingOffset(maxTenants, maxIlks, maxIchs int) int {
	tenantOffset := identityAlignUp(identityHeaderSize, identityRegionAlign)
	ilkOffset := identityAlignUp(tenantOffset+tenantEntrySize*maxTenants, identityRegionAlign)
	ichOffset := identityAlignUp(ilkOffset+ilkEntrySize*maxIlks, identityRegionAlign)
	return identityAlignUp(ichOffset+ichEntrySize*maxIchs, identityRegionAlign)
}

func identityAlignUp(n, align int) int {
	return (n + align - 1) &^ (align - 1)
}

// identityIchHash is FNV-1a 64-bit over channel_type ++ 0xff ++ address ++ 0xfe ++ tenant_id,
// matching compute_ich_hash in shm/mod.rs.
func identityIchHash(channelType, address string, tenantID [16]byte) uint64 {
	const basis = uint64(0xcbf29ce484222325)
	const prime = uint64(0x100000001b3)
	h := basis
	for _, b := range []byte(channelType) {
		h ^= uint64(b)
		h *= prime
	}
	h ^= 0xff
	h *= prime
	for _, b := range []byte(address) {
		h ^= uint64(b)
		h *= prime
	}
	h ^= 0xfe
	h *= prime
	for _, b := range tenantID {
		h ^= uint64(b)
		h *= prime
	}
	return h
}

// parseTenantUUIDBytes parses "tnt:<uuid>" (or bare UUID) into a 16-byte UUID.
// Returns zeroes on parse failure.
func parseTenantUUIDBytes(tenantID string) [16]byte {
	s := strings.TrimSpace(tenantID)
	s = strings.TrimPrefix(s, "tnt:")
	u, err := uuid.Parse(s)
	if err != nil {
		return [16]byte{}
	}
	return [16]byte(u)
}

// identityFixedStrMatches checks whether the null-terminated fixed-length byte
// buffer equals value.
func identityFixedStrMatches(buf []byte, value string) bool {
	end := len(buf)
	for i, b := range buf {
		if b == 0 {
			end = i
			break
		}
	}
	return string(buf[:end]) == value
}

// shmFilePath converts a POSIX SHM name to a Linux /dev/shm file path.
// On Linux, shm_open("/foo") maps to /dev/shm/foo.
func shmFilePath(name string) string {
	return "/dev/shm/" + strings.TrimPrefix(name, "/")
}

// LookupIlkByHandlerNode resolves the active ILK whose `handler_node` matches
// the given L2 name (e.g. "SY.timer@motherbee") from the hive's identity SHM.
// Returns ("ilk:<uuid>", true, nil) on hit, ("", false, nil) on miss.
func LookupIlkByHandlerNode(hiveID, handlerNode string) (string, bool, error) {
	name := fmt.Sprintf("/jsr-identity-%s", strings.TrimSpace(hiveID))
	return lookupIlkByHandlerNodeShm(name, handlerNode)
}

func lookupIlkByHandlerNodeShm(shmName, handlerNode string) (string, bool, error) {
	handler := strings.TrimSpace(handlerNode)
	if handler == "" {
		return "", false, nil
	}
	var ilkBytes [16]byte
	var found bool
	read, err := readIlkEntries(shmFilePath(shmName), func(entries []byte, count int) {
		ilkBytes, found = scanIlkEntriesByHandler(entries, count, handler)
	})
	if err != nil || !read || !found {
		return "", false, err
	}
	return "ilk:" + uuid.UUID(ilkBytes).String(), true, nil
}

// LookupIlk is what the identity memory of hiveID says of the active ILK ilkID ("ilk:<uuid>"): its
// type (human, agent or system) and its display name. (_, false, nil) on a miss, and when the region
// is absent.
func LookupIlk(hiveID, ilkID string) (IlkFacts, bool, error) {
	name := fmt.Sprintf("/jsr-identity-%s", strings.TrimSpace(hiveID))
	return lookupIlkAt(shmFilePath(name), ilkID)
}

func lookupIlkAt(path, ilkID string) (IlkFacts, bool, error) {
	wanted, err := uuid.Parse(strings.TrimPrefix(strings.TrimSpace(ilkID), "ilk:"))
	if err != nil {
		return IlkFacts{}, false, nil
	}
	var facts IlkFacts
	var found bool
	read, err := readIlkEntries(path, func(entries []byte, count int) {
		facts, found = scanIlkEntriesByID(entries, count, [16]byte(wanted))
	})
	if err != nil || !read || !found {
		return IlkFacts{}, false, err
	}
	return facts, true, nil
}

// readIlkEntries maps the identity memory at path and runs scan over its ILK entries under the
// region's seqlock: scan runs again when a writer moved the region meanwhile, for
// identitySeqlockTimeoutMs at most. False when the region is absent, not of this SDK's version, or
// still moving at the deadline.
func readIlkEntries(path string, scan func(entries []byte, count int)) (bool, error) {
	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, fmt.Errorf("identity shm open %q: %w", path, err)
	}
	defer f.Close()
	fi, err := f.Stat()
	if err != nil {
		return false, fmt.Errorf("identity shm stat: %w", err)
	}
	size := int(fi.Size())
	if size < identityHeaderSize {
		return false, nil
	}
	data, err := syscall.Mmap(int(f.Fd()), 0, size, syscall.PROT_READ, syscall.MAP_SHARED)
	if err != nil {
		return false, fmt.Errorf("identity shm mmap: %w", err)
	}
	defer syscall.Munmap(data) //nolint:errcheck

	magic := binary.LittleEndian.Uint32(data[hdrOffMagic:])
	if magic != identityMagic {
		return false, nil
	}
	version := binary.LittleEndian.Uint32(data[hdrOffVersion:])
	if version != identityVersionCurrent {
		return false, nil
	}

	ilkCount := int(binary.LittleEndian.Uint32(data[hdrOffIlkCount:]))
	maxIlks := int(binary.LittleEndian.Uint32(data[hdrOffMaxIlks:]))
	maxTenants := int(binary.LittleEndian.Uint32(data[hdrOffMaxTenants:]))
	if maxIlks <= 0 {
		return false, nil
	}
	if ilkCount > maxIlks {
		ilkCount = maxIlks
	}

	ilkOffset := identityAlignUp(identityHeaderSize, identityRegionAlign)
	ilkOffset = identityAlignUp(ilkOffset+tenantEntrySize*maxTenants, identityRegionAlign)
	needed := ilkOffset + ilkCount*ilkEntrySize
	if needed > size {
		return false, nil
	}

	seqPtr := (*uint64)(unsafe.Pointer(&data[hdrOffSeq]))
	deadline := time.Now().Add(identitySeqlockTimeoutMs * time.Millisecond)
	for {
		s1 := atomic.LoadUint64(seqPtr)
		if s1&1 != 0 {
			runtime.Gosched()
			if time.Now().After(deadline) {
				return false, nil
			}
			continue
		}
		scan(data[ilkOffset:], ilkCount)
		s2 := atomic.LoadUint64(seqPtr)
		if s1 == s2 {
			return true, nil
		}
		if time.Now().After(deadline) {
			return false, nil
		}
		runtime.Gosched()
	}
}

// scanIlkEntriesByID finds the active entry of id: its type and its display name.
func scanIlkEntriesByID(buf []byte, count int, id [16]byte) (IlkFacts, bool) {
	for i := 0; i < count; i++ {
		off := i * ilkEntrySize
		if off+ilkEntrySize > len(buf) {
			break
		}
		entry := buf[off : off+ilkEntrySize]
		if binary.LittleEndian.Uint16(entry[ilkOffFlags:])&ilkFlagActive == 0 {
			continue
		}
		if !bytes.Equal(entry[ilkOffIlkID:ilkOffIlkID+16], id[:]) {
			continue
		}
		return IlkFacts{
			Type: ilkTypeName(entry[ilkOffType]),
			Name: identityFixedStr(entry[ilkOffDisplayName : ilkOffDisplayName+128]),
		}, true
	}
	return IlkFacts{}, false
}

// ilkTypeName is an entry's ilk_type as the identity names it (fluxbee_sdk ilk_type_from_shm).
func ilkTypeName(value byte) string {
	switch value {
	case 0:
		return "human"
	case 1:
		return "agent"
	case 2:
		return "system"
	default:
		return "unknown"
	}
}

// identityFixedStr is a null-terminated fixed-length buffer's text.
func identityFixedStr(buf []byte) string {
	if end := bytes.IndexByte(buf, 0); end >= 0 {
		buf = buf[:end]
	}
	return strings.TrimSpace(string(buf))
}

func scanIlkEntriesByHandler(buf []byte, count int, handler string) ([16]byte, bool) {
	var zero [16]byte
	for i := 0; i < count; i++ {
		off := i * ilkEntrySize
		if off+ilkEntrySize > len(buf) {
			return zero, false
		}
		entry := buf[off : off+ilkEntrySize]
		flags := binary.LittleEndian.Uint16(entry[ilkOffFlags:])
		if flags&ilkFlagActive == 0 {
			continue
		}
		handlerBuf := entry[ilkOffHandlerNode : ilkOffHandlerNode+128]
		if !identityFixedStrMatches(handlerBuf, handler) {
			continue
		}
		var ilkID [16]byte
		copy(ilkID[:], entry[ilkOffIlkID:ilkOffIlkID+16])
		return ilkID, true
	}
	return zero, false
}

// WaitForSelfSystemIlkID blocks until the SY node's own ILK (seeded by SY.identity)
// is visible in identity SHM and returns its "ilk:<uuid>" string. selfBaseName is
// the L2 base (e.g. "SY.timer") — the hive is appended automatically.
func WaitForSelfSystemIlkID(hiveID, selfBaseName string, timeout, pollInterval time.Duration) (string, error) {
	handler := strings.TrimSpace(selfBaseName)
	if handler == "" {
		return "", fmt.Errorf("self base name is empty")
	}
	if !strings.Contains(handler, "@") {
		handler = fmt.Sprintf("%s@%s", handler, strings.TrimSpace(hiveID))
	}
	deadline := time.Now().Add(timeout)
	for {
		ilkID, ok, err := LookupIlkByHandlerNode(hiveID, handler)
		if err != nil {
			return "", err
		}
		if ok {
			return ilkID, nil
		}
		if time.Now().After(deadline) {
			return "", fmt.Errorf("self system ILK not visible for handler_node=%q after %s", handler, timeout)
		}
		time.Sleep(pollInterval)
	}
}
