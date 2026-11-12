package sdk

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// textPayload is a text/v1 payload of content, with extra fields.
func textPayload(t *testing.T, content string, extra map[string]any) json.RawMessage {
	t.Helper()
	fields := map[string]any{"type": "text", "content": content, "attachments": []any{}}
	for key, value := range extra {
		fields[key] = value
	}
	raw, err := marshalNoEscape(fields)
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	return raw
}

// textMessage is a WF's user message to an IO node, its payload a text/v1 payload of content with
// extra fields.
func textMessage(t *testing.T, content string, extra map[string]any) Message {
	t.Helper()
	return Message{
		Routing: Routing{Src: "wf-uuid", Dst: UnicastDestination("IO.slack.demo@motherbee"), TTL: 16, TraceID: "trace-text"},
		Meta:    Meta{MsgType: "user", Msg: stringPtr("REPORT")},
		Payload: textPayload(t, content, extra),
	}
}

// sendText sends msg as a WF's sender does, its blobs written to root: the frame it queued, or why it
// queued none.
func sendText(t *testing.T, root string, msg Message) ([]byte, error) {
	t.Helper()
	saved := textOffloadBlobRoot
	textOffloadBlobRoot = root
	defer func() { textOffloadBlobRoot = saved }()
	tx := make(chan []byte, 1)
	sender := &NodeSender{uuid: "wf-uuid", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}}
	if err := sender.Send(msg); err != nil {
		if len(tx) != 0 {
			t.Fatalf("a message refused with %v was queued", err)
		}
		return nil, err
	}
	return readFrame(t, tx), nil
}

// sentFields is the payload of frame, field by field.
func sentFields(t *testing.T, frame []byte) map[string]json.RawMessage {
	t.Helper()
	var sent Message
	if err := json.Unmarshal(frame, &sent); err != nil {
		t.Fatalf("decode the frame: %v", err)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(sent.Payload, &fields); err != nil {
		t.Fatalf("decode the payload: %v", err)
	}
	return fields
}

// sentRef is the content_ref of frame's payload, a text/v1 payload whose text went as a blob, which
// carries no content.
func sentRef(t *testing.T, frame []byte) BlobRef {
	t.Helper()
	fields := sentFields(t, frame)
	if _, ok := fields["content"]; ok {
		t.Fatalf("content and content_ref are exclusive: %s", frame)
	}
	var ref BlobRef
	if err := json.Unmarshal(fields["content_ref"], &ref); err != nil || ref.BlobName == "" {
		t.Fatalf("want a content_ref: %s (%v)", fields["content_ref"], err)
	}
	return ref
}

// readBlob reads root's active blob ref names, and checks it is the blob ref says, as the Rust SDK's
// read_blob does: its size, and the hash in its name.
func readBlob(t *testing.T, root string, ref BlobRef) []byte {
	t.Helper()
	path := filepath.Join(root, "active", blobPrefix(ref.BlobName), ref.BlobName)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read the blob: %v", err)
	}
	sum := sha256.Sum256(data)
	if uint64(len(data)) != ref.Size || !strings.Contains(ref.BlobName, "_"+hex.EncodeToString(sum[:])[:16]+".") {
		t.Fatalf("the blob is not the one its ref names: %d bytes, %+v", len(data), ref)
	}
	info, err := os.Stat(path)
	if err != nil || info.Mode().Perm() != 0o640 {
		t.Fatalf("a blob is 0640: %v %v", info.Mode(), err)
	}
	return data
}

// nothingWritten fails t when anything was written under root.
func nothingWritten(t *testing.T, root string) {
	t.Helper()
	if entries, _ := os.ReadDir(root); len(entries) != 0 {
		t.Fatalf("nothing is written: %v", entries)
	}
}

// FINDINGS A-264, the operator's call (b): a message that fits the frame goes as it is, byte for
// byte, its text inline however long, and writes nothing: what went inline before 0.1.141 still
// does, the same frame, and reaches its reader on any hive as it did (the router's additions, the
// sender's name and the action class, can still take a frame this close to the limit over it, and
// the router refuses it, as before: FINDINGS A-202). Built first at the Rust SDK's threshold (about
// 62 KiB), such a text went as a blob, which a reader on another hive may not find yet. One byte
// over the frame, the text goes as a blob.
func TestATextThatFitsTheFrameGoesInline(t *testing.T) {
	root := t.TempDir()
	empty, err := sendText(t, root, textMessage(t, "", nil))
	if err != nil {
		t.Fatalf("send: %v", err)
	}
	fill := maxFrameSize - len(empty)
	msg := textMessage(t, strings.Repeat("y", fill), nil)
	frame, err := sendText(t, root, msg)
	if err != nil || len(frame) != maxFrameSize {
		t.Fatalf("a text whose frame is the limit goes inline: a frame of %d bytes, %v", len(frame), err)
	}
	var sent Message
	if err := json.Unmarshal(frame, &sent); err != nil || !bytes.Equal(sent.Payload, msg.Payload) {
		t.Fatalf("the payload goes byte for byte (%v)", err)
	}
	nothingWritten(t, root)

	frame, err = sendText(t, root, textMessage(t, strings.Repeat("y", fill+1), nil))
	if err != nil {
		t.Fatalf("one byte over the frame: %v", err)
	}
	if ref := sentRef(t, frame); ref.Size != uint64(fill+1) {
		t.Fatalf("one byte over the frame goes as a blob of the text: %+v", ref)
	}
}

// FINDINGS A-264: a WF's text whose message is over the frame goes as a blob: its content becomes
// content_ref, a blob content_<hash>.txt, text/plain, of the UTC day, in active/<prefix>/ of the
// blob root, nothing left in staging/; the payload's other fields go as they came. Before 0.1.141
// such a message was refused (MessageTooLargeError), and the text was never sent.
func TestATextOverTheFrameGoesAsABlob(t *testing.T) {
	root := t.TempDir()
	content := strings.Repeat("a report line\n", 15000)
	msg := textMessage(t, content, map[string]any{"menu": map[string]any{"options": []string{"yes", "no"}}})
	if len(msg.Payload) <= maxFrameSize {
		t.Fatalf("the text must be over the frame: %d bytes", len(msg.Payload))
	}
	before := time.Now().UTC().Format("2006-01-02")
	frame, err := sendText(t, root, msg)
	if err != nil {
		var tooLarge *MessageTooLargeError
		if errors.As(err, &tooLarge) {
			t.Fatalf("a text over the frame was refused instead of sent as a blob: %v", err)
		}
		t.Fatalf("send: %v", err)
	}
	if len(frame) > 4096 {
		t.Fatalf("the frame carries the ref, not the text: %d bytes", len(frame))
	}
	got := sentRef(t, frame)
	day := before
	if after := time.Now().UTC().Format("2006-01-02"); got.SpoolDay == after {
		day = after // the send crossed midnight, UTC
	}
	sum := sha256.Sum256([]byte(content))
	want := BlobRef{
		Type: "blob_ref", BlobName: "content_" + hex.EncodeToString(sum[:])[:16] + ".txt", Size: uint64(len(content)),
		Mime: "text/plain", FilenameOriginal: "content.txt", SpoolDay: day,
	}
	if got != want {
		t.Fatalf("got %+v, want %+v", got, want)
	}
	fields := sentFields(t, frame)
	if string(fields["type"]) != `"text"` || string(fields["attachments"]) != `[]` ||
		string(fields["menu"]) != `{"options":["yes","no"]}` {
		t.Fatalf("the other fields go as they came: %s", frame)
	}
	if data := readBlob(t, root, got); string(data) != content {
		t.Fatalf("the blob holds another text")
	}
	staged, err := os.ReadDir(filepath.Join(root, "staging", blobPrefix(got.BlobName)))
	if err != nil || len(staged) != 0 {
		t.Fatalf("nothing is left in staging: %v %v", staged, err)
	}
}

// FINDINGS A-264: a message over the frame whose payload is no text/v1 text is refused as before
// (MessageTooLargeError), and writes nothing: one with a content_ref already, one that is no text/v1
// payload, one whose content is no string, null (the review: it went as an empty blob, its
// content_ref of size 0) or missing, and one that is no object.
func TestAMessageOverTheFrameWithNoTextIsRefusedAsBefore(t *testing.T) {
	root := t.TempDir()
	big := strings.Repeat("x", maxFrameSize)
	for name, payload := range map[string]json.RawMessage{
		"a text with a content_ref": json.RawMessage(`{"type":"text","content_ref":{"type":"blob_ref","blob_name":"content_0123456789abcdef.txt","size":1,"mime":"text/plain","filename_original":"content.txt","spool_day":"2026-11-12"},"pad":"` + big + `"}`),
		"no text":                   json.RawMessage(`{"type":"admin","content":"` + big + `"}`),
		"a content that is no text": json.RawMessage(`{"type":"text","content":{"x":"` + big + `"}}`),
		"a content that is null":    json.RawMessage(`{"type":"text","content":null,"pad":"` + big + `"}`),
		"no content":                json.RawMessage(`{"type":"text","pad":"` + big + `"}`),
		"no object":                 json.RawMessage(`"` + big + `"`),
	} {
		if _, ok, err := planTextBlob(payload, time.Now()); ok || err != nil {
			t.Fatalf("%s: planned as a blob (%v)", name, err)
		}
		msg := textMessage(t, "", nil)
		msg.Payload = payload
		_, err := sendText(t, root, msg)
		var tooLarge *MessageTooLargeError
		if !errors.As(err, &tooLarge) {
			t.Fatalf("%s: want it refused as before, got %v", name, err)
		}
	}
	nothingWritten(t, root)
}

// FINDINGS A-264: a message still over the frame with its text's ref in place of the text (its
// meta.context, a WF's resolved vars, over it) is refused as before, and writes no blob: none that no
// message names, which Syncthing would copy to every hive all the same.
func TestAMessageOverTheFrameWithItsRefTooWritesNoBlob(t *testing.T) {
	root := t.TempDir()
	msg := textMessage(t, strings.Repeat("z", maxFrameSize), nil)
	msg.Meta.Context = json.RawMessage(`{"vars":{"report":"` + strings.Repeat("v", maxFrameSize) + `"}}`)
	_, err := sendText(t, root, msg)
	var tooLarge *MessageTooLargeError
	if !errors.As(err, &tooLarge) {
		t.Fatalf("want it refused as before, got %v", err)
	}
	nothingWritten(t, root)
}

// FINDINGS A-264: a text whose first bytes are an HTML or SVG document is labeled as such, so it is
// served sandboxed, as the Rust SDK labels it (FIX-8); markup in the middle of a text is not.
func TestActiveMarkupIsLabeledAsTheRustSDKLabelsIt(t *testing.T) {
	for text, want := range map[string]string{
		"plain text": "",
		"\xEF\xBB\xBF \n\t<!DOCTYPE HTML><p>x</p>": "text/html",
		"<Script>alert(1)</script>":                "text/html",
		"<?xml version=\"1.0\"?>\n<svg></svg>":     "image/svg+xml",
		"  <SVG width=\"1\">":                      "image/svg+xml",
		"{\"note\": \"<script>x</script>\"}":       "",
		"\u00a0<html>":                             "",
		string([]byte{0xA0}) + "<html>":            "text/html",
	} {
		if got := sniffActiveContent([]byte(text)); got != want {
			t.Fatalf("%q: got %q, want %q", text, got, want)
		}
	}
	root := t.TempDir()
	frame, err := sendText(t, root, textMessage(t, "<!doctype html>"+strings.Repeat("<p>page</p>", 12000), nil))
	if err != nil {
		t.Fatalf("send: %v", err)
	}
	if ref := sentRef(t, frame); ref.Mime != "text/html" {
		t.Fatalf("an HTML text goes as text/html: %+v", ref)
	}
}

// FINDINGS A-264: the same text twice is one blob: its name is its hash, and the second promote
// finds it in place and leaves it, never replacing a blob a reader or Syncthing may be reading.
func TestTheSameTextTwiceIsOneBlob(t *testing.T) {
	root := t.TempDir()
	msg := textMessage(t, strings.Repeat("same ", 30000), nil)
	frame, err := sendText(t, root, msg)
	if err != nil {
		t.Fatalf("first: %v", err)
	}
	first := sentRef(t, frame)
	path := filepath.Join(root, "active", blobPrefix(first.BlobName), first.BlobName)
	before, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	frame, err = sendText(t, root, msg)
	if err != nil {
		t.Fatalf("second: %v", err)
	}
	if second := sentRef(t, frame); second != first {
		t.Fatalf("second: %+v, first: %+v", second, first)
	}
	entries, err := os.ReadDir(filepath.Join(root, "active", blobPrefix(first.BlobName)))
	if err != nil || len(entries) != 1 {
		t.Fatalf("one blob: %v %v", entries, err)
	}
	if after, err := os.Stat(path); err != nil || !os.SameFile(before, after) {
		t.Fatalf("the blob in place was replaced: %v", err)
	}
}

// FINDINGS A-264, A-56: the offload never writes through a link planted in the blob tree; it fails,
// and the message is refused as before.
func TestALinkInTheBlobTreeIsNeverFollowed(t *testing.T) {
	for _, planted := range []string{"staging", "active"} {
		t.Run(planted, func(t *testing.T) {
			root := t.TempDir()
			elsewhere := t.TempDir()
			if err := os.Symlink(elsewhere, filepath.Join(root, planted)); err != nil {
				t.Fatalf("plant: %v", err)
			}
			_, err := sendText(t, root, textMessage(t, strings.Repeat("z", maxFrameSize), nil))
			var tooLarge *MessageTooLargeError
			if !errors.As(err, &tooLarge) {
				t.Fatalf("want the message refused as before, got %v", err)
			}
			if entries, _ := os.ReadDir(elsewhere); len(entries) != 0 {
				t.Fatalf("wrote through the link: %v", entries)
			}
		})
	}
}
