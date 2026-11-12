package sdk

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"strings"
	"time"
	"unicode"

	"github.com/google/uuid"
	"golang.org/x/sys/unix"
)

// The text/v1 offload (FINDINGS A-264): as it sends, the SDK puts the text of a text/v1 payload
// whose inline form would be over textOffloadMaxMessageBytes, with textOffloadOverheadBytes for the
// rest of the frame, in a blob of the hive's blob store, and sends the blob's ref in its place
// (content_ref), as the Rust SDK does (crates/fluxbee_sdk/src/send_normalization.rs). The blob's
// layout and its promote are the Rust SDK's (crates/fluxbee_sdk/src/blob): staging/<prefix>/<name>
// written to an exclusive temporary file and renamed, then moved to active/<prefix>/<name>, given
// to the owner of active/ (the Syncthing user, who copies it to the other hives), every directory
// opened without following a link (FINDINGS A-56). A reader (an IO node, an agent) reads it with
// the Rust SDK's read_blob_with_retry, which checks its size and the hash in its name. Before
// 0.1.141 a WF's text over 128 KiB was never sent (MessageTooLargeError), and one between about 96
// and 128 KiB reached its node but not the router stream's turns (FINDINGS A-253).
//
// Unlike the Rust SDK, a payload under the limit goes byte for byte, and one over it keeps its
// other fields: only content becomes content_ref. When the blob cannot be written, the payload
// goes as it is, with a log line, as in Rust.

const (
	// TextOffloadMaxMessageBytes is the most an inline text/v1 payload, with
	// TextOffloadOverheadBytes, may take before its text goes as a blob: the Rust SDK's
	// TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES.
	TextOffloadMaxMessageBytes = 64 * 1024
	// TextOffloadOverheadBytes is what the rest of the frame is counted as: the Rust SDK's
	// TEXT_V1_DEFAULT_OVERHEAD_BYTES.
	TextOffloadOverheadBytes = 2048
	// DefaultBlobRoot is the hive's blob store, the Rust SDK's default.
	DefaultBlobRoot = "/var/lib/fluxbee/blob"

	blobHashLen   = 16
	blobPrefixLen = 2
	// textBlobFilename and textBlobMime are what the Rust SDK names a text it offloads.
	textBlobFilename = "content.txt"
	textBlobMime     = "text/plain"
)

// textOffloadBlobRoot is where SendTraced writes the blobs of the texts it offloads; tests change
// it.
var textOffloadBlobRoot = DefaultBlobRoot

// BlobRef names a blob of the hive's blob store, as a payload carries it: the Rust SDK's BlobRef
// (docs/reference/protocol.md §8). BlobName is <name>_<the first 16 hex digits of the SHA-256 of its
// bytes>.<ext>; SpoolDay the UTC day it was written, YYYY-MM-DD.
type BlobRef struct {
	Type             string `json:"type"`
	BlobName         string `json:"blob_name"`
	Size             uint64 `json:"size"`
	Mime             string `json:"mime"`
	FilenameOriginal string `json:"filename_original"`
	SpoolDay         string `json:"spool_day"`
}

// offloadText returns payload, a text/v1 payload, with its content in a blob of root when its
// inline form is over the limit; any other payload, one under the limit, one that has a content_ref
// already, or one that is not a text/v1 payload with a text (its content missing, null or no
// string), comes back as it is, with no ref. An error says the text was over the limit and could
// not go as a blob: payload comes back as it is.
func offloadText(root string, payload json.RawMessage) (json.RawMessage, *BlobRef, error) {
	if len(payload)+TextOffloadOverheadBytes <= TextOffloadMaxMessageBytes {
		return payload, nil, nil
	}
	var fields map[string]json.RawMessage
	if json.Unmarshal(payload, &fields) != nil {
		return payload, nil, nil
	}
	var kind string
	if json.Unmarshal(fields["type"], &kind) != nil || kind != "text" {
		return payload, nil, nil
	}
	if ref, ok := fields["content_ref"]; ok && string(ref) != "null" {
		return payload, nil, nil
	}
	// A null content is no text: the payload goes as it is, as the Rust SDK sends a text/v1 payload
	// whose content is None (send_normalization.rs), never as an empty blob.
	var content *string
	if json.Unmarshal(fields["content"], &content) != nil || content == nil {
		return payload, nil, nil
	}
	ref, err := putTextBlob(root, []byte(*content))
	if err != nil {
		return payload, nil, err
	}
	raw, err := json.Marshal(ref)
	if err != nil {
		return payload, nil, err
	}
	delete(fields, "content")
	fields["content_ref"] = raw
	out, err := marshalNoEscape(fields)
	if err != nil {
		return payload, nil, err
	}
	return out, &ref, nil
}

// offloadMessageText is offloadText on msg's payload, as SendTraced sends it, with its log lines.
func offloadMessageText(msg *Message) {
	payload, ref, err := offloadText(textOffloadBlobRoot, msg.Payload)
	switch {
	case err != nil:
		log.Printf("fluxbee-go-sdk: text/v1 offload failed, the payload goes as it is: %v (msg_type=%q msg=%q trace_id=%s)",
			err, msg.Meta.MsgType, stringValue(msg.Meta.Msg), msg.Routing.TraceID)
	case ref != nil:
		log.Printf("fluxbee-go-sdk: text/v1 content of %d bytes sent as blob %s (msg_type=%q msg=%q trace_id=%s)",
			ref.Size, ref.BlobName, msg.Meta.MsgType, stringValue(msg.Meta.Msg), msg.Routing.TraceID)
		msg.Payload = payload
	}
}

// marshalNoEscape is value's JSON without HTML escaping, as a frame carries it.
func marshalNoEscape(value any) (json.RawMessage, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(value); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// putTextBlob writes data, a text, as a blob of root (content.txt, text/plain unless its bytes are
// active markup), staged and promoted as the Rust SDK's put_bytes and promote do.
func putTextBlob(root string, data []byte) (BlobRef, error) {
	sum := sha256.Sum256(data)
	hash := hex.EncodeToString(sum[:])
	mime := textBlobMime
	if active := sniffActiveContent(data); active != "" {
		mime = active
	}
	ref := BlobRef{
		Type:             "blob_ref",
		BlobName:         "content_" + hash[:blobHashLen] + ".txt",
		Size:             uint64(len(data)),
		Mime:             mime,
		FilenameOriginal: textBlobFilename,
		SpoolDay:         time.Now().UTC().Format("2006-01-02"),
	}
	if err := writeStagedBlob(root, ref.BlobName, data); err != nil {
		return BlobRef{}, fmt.Errorf("write staging file: %w", err)
	}
	if err := promoteBlob(root, ref.BlobName); err != nil {
		return BlobRef{}, fmt.Errorf("promote staging->active: %w", err)
	}
	return ref, nil
}

// blobPrefix is the directory of a blob under staging/ and active/: the first two hex digits of
// the hash in its name.
func blobPrefix(name string) string {
	underscore := strings.LastIndexByte(name, '_')
	if underscore < 0 || len(name) < underscore+1+blobPrefixLen {
		return "00"
	}
	return name[underscore+1 : underscore+1+blobPrefixLen]
}

// sniffActiveContent is the Rust SDK's sniff_active_content: the active MIME a blob is served as
// when its first bytes, past a UTF-8 BOM and white space, are an SVG or HTML document's
// (image/svg+xml or text/html), so it is served sandboxed; "" for any other bytes.
func sniffActiveContent(data []byte) string {
	data = bytes.TrimPrefix(data, []byte{0xEF, 0xBB, 0xBF})
	if len(data) > 512 {
		data = data[:512]
	}
	head := make([]rune, len(data))
	for i, b := range data {
		if 'A' <= b && b <= 'Z' {
			b += 'a' - 'A'
		}
		head[i] = rune(b)
	}
	text := string(head)
	trimmed := strings.TrimLeftFunc(text, unicode.IsSpace)
	if strings.HasPrefix(trimmed, "<svg") || (strings.HasPrefix(trimmed, "<?xml") && strings.Contains(text, "<svg")) {
		return "image/svg+xml"
	}
	for _, marker := range []string{"<!doctype html", "<html", "<head", "<body", "<script", "<iframe"} {
		if strings.HasPrefix(trimmed, marker) {
			return "text/html"
		}
	}
	return ""
}

// writeStagedBlob writes data as staging/<prefix>/<name> of root: each directory opened without
// following a link, the bytes written to an exclusive temporary file, 0640, then renamed over the
// name. staging/ is root's; nothing here acts through a name another user could plant (FINDINGS
// A-56; the Rust SDK's write_staging).
func writeStagedBlob(root, name string, data []byte) error {
	if err := os.MkdirAll(root, 0o755); err != nil {
		return err
	}
	rootFD, err := openDirNoFollow(unix.AT_FDCWD, root)
	if err != nil {
		return err
	}
	defer unix.Close(rootFD)
	staging, err := ensureChildDir(rootFD, "staging", nil)
	if err != nil {
		return err
	}
	defer unix.Close(staging)
	dir, err := ensureChildDir(staging, blobPrefix(name), nil)
	if err != nil {
		return err
	}
	defer unix.Close(dir)
	temp := fmt.Sprintf(".%s.%s.tmp", name, strings.ReplaceAll(uuid.NewString(), "-", ""))
	fd, err := unix.Openat(dir, temp, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0o640)
	if err != nil {
		return err
	}
	file := os.NewFile(uintptr(fd), temp)
	_, err = file.Write(data)
	if err == nil {
		err = file.Chmod(0o640)
	}
	if closeErr := file.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		err = unix.Renameat(dir, temp, dir, name)
	}
	if err != nil {
		_ = unix.Unlinkat(dir, temp, 0)
	}
	return err
}

// promoteBlob moves staging/<prefix>/<name> of root to active/<prefix>/<name>, the blob and a prefix
// directory made here given to the owner of active/ first, so the Syncthing that serves the folder
// can read them (the Rust SDK's move_staged_to_active). A regular file already under the name in
// active/ is the same blob (its name is its hash): nothing to move.
func promoteBlob(root, name string) error {
	if err := os.MkdirAll(root, 0o755); err != nil {
		return err
	}
	rootFD, err := openDirNoFollow(unix.AT_FDCWD, root)
	if err != nil {
		return err
	}
	defer unix.Close(rootFD)
	active, err := ensureChildDir(rootFD, "active", nil)
	if err != nil {
		return err
	}
	defer unix.Close(active)
	owner, err := ownerOf(active)
	if err != nil {
		return err
	}
	prefix := blobPrefix(name)
	dir, err := ensureChildDir(active, prefix, &owner)
	if err != nil {
		return err
	}
	defer unix.Close(dir)
	if present, err := regularFileAt(dir, name); err != nil || present {
		return err
	}
	// Nothing staged any more is fine when a promote of the same blob moved it already.
	promotedAlready := func(err error) error {
		if errors.Is(err, unix.ENOENT) {
			if present, _ := regularFileAt(dir, name); present {
				return nil
			}
		}
		return err
	}
	staging, err := openDirNoFollow(rootFD, "staging")
	if err != nil {
		return promotedAlready(err)
	}
	defer unix.Close(staging)
	stagedDir, err := openDirNoFollow(staging, prefix)
	if err != nil {
		return promotedAlready(err)
	}
	defer unix.Close(stagedDir)
	staged, err := openFileNoFollow(stagedDir, name)
	if err != nil {
		return promotedAlready(err)
	}
	defer unix.Close(staged)
	if err := handOverStagedBlob(staged, owner); err != nil {
		return err
	}
	held, err := identityOf(staged)
	if err != nil {
		return err
	}
	if err := unix.Renameat(stagedDir, name, dir, name); err != nil {
		return promotedAlready(err)
	}
	// Renameat moves a name: a put of the same blob can replace the staged file between the open
	// above and the move, so the inode that landed may not be the one handed over.
	landed, err := openFileNoFollow(dir, name)
	if err != nil {
		return err
	}
	defer unix.Close(landed)
	if identity, err := identityOf(landed); err != nil || identity != held {
		if err != nil {
			return err
		}
		return handOverStagedBlob(landed, owner)
	}
	return nil
}

// fileOwner is a file's (uid, gid).
type fileOwner struct{ uid, gid int }

// openDirNoFollow opens name as a directory, relative to parent (unix.AT_FDCWD for none), never
// through a link at its last component.
func openDirNoFollow(parent int, name string) (int, error) {
	return unix.Openat(parent, name, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
}

// openFileNoFollow opens name under parent to read, never through a link, and without blocking on
// a FIFO planted under the name.
func openFileNoFollow(parent int, name string) (int, error) {
	return unix.Openat(parent, name, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
}

// ensureChildDir is name under parent, made 0750 when missing, opened without following a link,
// and given to owner when one is named and it has another. An existing directory keeps its mode.
func ensureChildDir(parent int, name string, owner *fileOwner) (int, error) {
	created := true
	if err := unix.Mkdirat(parent, name, 0o750); err != nil {
		if !errors.Is(err, unix.EEXIST) {
			return -1, err
		}
		created = false
	}
	fd, err := openDirNoFollow(parent, name)
	if err != nil {
		return -1, err
	}
	if owner != nil {
		if err := giveTo(fd, *owner); err != nil {
			unix.Close(fd)
			return -1, err
		}
	}
	if created {
		// Mkdirat's mode went through the umask.
		if err := unix.Fchmod(fd, 0o750); err != nil {
			unix.Close(fd)
			return -1, err
		}
	}
	return fd, nil
}

// ownerOf is the owner of an open file or directory.
func ownerOf(fd int) (fileOwner, error) {
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return fileOwner{}, err
	}
	return fileOwner{uid: int(st.Uid), gid: int(st.Gid)}, nil
}

// giveTo gives fd to owner when it has another. Nothing to do, and no privilege needed, when it
// already has that owner; otherwise only root can, and failing is an error: a blob the Syncthing
// user cannot read would never sync.
func giveTo(fd int, owner fileOwner) error {
	current, err := ownerOf(fd)
	if err != nil || current == owner {
		return err
	}
	return unix.Fchown(fd, owner.uid, owner.gid)
}

// regularFileAt reports whether name under dir is a regular file, without following a link.
func regularFileAt(dir int, name string) (bool, error) {
	var st unix.Stat_t
	if err := unix.Fstatat(dir, name, &st, unix.AT_SYMLINK_NOFOLLOW); err != nil {
		if errors.Is(err, unix.ENOENT) {
			return false, nil
		}
		return false, err
	}
	return uint32(st.Mode)&unix.S_IFMT == unix.S_IFREG, nil
}

// identityOf is an open file's (device, inode).
func identityOf(fd int) ([2]uint64, error) {
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return [2]uint64{}, err
	}
	return [2]uint64{uint64(st.Dev), uint64(st.Ino)}, nil
}

// handOverStagedBlob gives a staged blob to owner, 0640, once it is one root may give away: a
// regular file this process wrote (one link, its own), one a concurrent put of the same blob
// replaced (no link left), or one a concurrent promote handed to owner already. Root never gives
// away anything else (the Rust SDK's is_handable_staged_file).
func handOverStagedBlob(fd int, owner fileOwner) error {
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return err
	}
	regular := uint32(st.Mode)&unix.S_IFMT == unix.S_IFREG
	links := uint64(st.Nlink)
	mine := int(st.Uid) == unix.Geteuid()
	owned := fileOwner{uid: int(st.Uid), gid: int(st.Gid)} == owner
	if !regular || (links != 0 && (links != 1 || !(mine || owned))) {
		return fmt.Errorf("BLOB_INTEGRITY: the staged blob is not a regular file this process staged")
	}
	if err := giveTo(fd, owner); err != nil {
		return err
	}
	return unix.Fchmod(fd, 0o640)
}
