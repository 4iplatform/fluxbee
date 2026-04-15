package sdk

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

	"github.com/google/uuid"
)

const (
	identityMagic          uint32 = 0x4A534944 // "JSID"
	identityVersionCurrent uint32 = 3
	identityRegionAlign           = 64
	identitySeqlockTimeoutMs      = 50

	// Struct sizes computed from #[repr(C)] Rust layout rules.
	identityHeaderSize = 176
	tenantEntrySize    = 312
	ilkEntrySize       = 344
	ichEntrySize       = 352
	ichMappingEntrySize = 384

	// Field offsets within IchMappingEntry (384 bytes total).
	ichMapOffHash        = 0   // u64
	ichMapOffChannelType = 8   // [u8; 32]
	ichMapOffAddress     = 40  // [u8; 256]
	ichMapOffIchID       = 296 // [u8; 16]
	ichMapOffIlkID       = 312 // [u8; 16]
	ichMapOffTenantID    = 328 // [u8; 16]
	ichMapOffFlags       = 344 // u16

	// Field offsets within IdentityHeader.
	hdrOffMagic          = 0
	hdrOffVersion        = 4
	hdrOffSeq            = 8  // AtomicU64
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
	start := int(hash%uint64(maxMappings))
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
