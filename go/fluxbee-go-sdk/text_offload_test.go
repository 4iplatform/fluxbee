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

// FINDINGS A-264: a text/v1 payload whose inline form is over 64 KiB, with 2 KiB for the rest of the
// frame, goes as a blob: its content becomes content_ref, a blob content_<hash>.txt, text/plain,
// of the UTC day, in active/<prefix>/ of the blob root, nothing left in staging/; its other fields go
// as they came.
func TestATextOverTheLimitGoesAsABlob(t *testing.T) {
	root := t.TempDir()
	content := strings.Repeat("A long answer. ", 5000)
	payload := textPayload(t, content, map[string]any{"menu": map[string]any{"options": []string{"yes", "no"}}})

	out, ref, err := offloadText(root, payload)
	if err != nil || ref == nil {
		t.Fatalf("want the text as a blob, got %v %v", ref, err)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(out, &fields); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if _, ok := fields["content"]; ok {
		t.Fatalf("content and content_ref are exclusive: %s", out)
	}
	var got BlobRef
	if err := json.Unmarshal(fields["content_ref"], &got); err != nil || got != *ref {
		t.Fatalf("content_ref: %s (%v)", fields["content_ref"], err)
	}
	sum := sha256.Sum256([]byte(content))
	want := BlobRef{
		Type: "blob_ref", BlobName: "content_" + hex.EncodeToString(sum[:])[:16] + ".txt", Size: uint64(len(content)),
		Mime: "text/plain", FilenameOriginal: "content.txt", SpoolDay: time.Now().UTC().Format("2006-01-02"),
	}
	if got != want {
		t.Fatalf("got %+v, want %+v", got, want)
	}
	if string(fields["type"]) != `"text"` || string(fields["attachments"]) != `[]` ||
		string(fields["menu"]) != `{"options":["yes","no"]}` {
		t.Fatalf("the other fields go as they came: %s", out)
	}
	if data := readBlob(t, root, got); string(data) != content {
		t.Fatalf("the blob holds another text")
	}
	staged, err := os.ReadDir(filepath.Join(root, "staging", blobPrefix(got.BlobName)))
	if err != nil || len(staged) != 0 {
		t.Fatalf("nothing is left in staging: %v %v", staged, err)
	}
	if len(out) > 1024 {
		t.Fatalf("the payload is a ref now, %d bytes", len(out))
	}
}

// FINDINGS A-264: a payload under the limit, one with a content_ref already, and one that is no
// text/v1 text go byte for byte, and write nothing.
func TestAPayloadThatNeedsNoBlobGoesByteForByte(t *testing.T) {
	root := t.TempDir()
	big := strings.Repeat("x", 70*1024)
	for name, payload := range map[string]json.RawMessage{
		"a text under the limit":    textPayload(t, strings.Repeat("y", TextOffloadMaxMessageBytes-TextOffloadOverheadBytes-100), nil),
		"a text with a content_ref": json.RawMessage(`{"type":"text","content_ref":{"type":"blob_ref","blob_name":"content_0123456789abcdef.txt","size":1,"mime":"text/plain","filename_original":"content.txt","spool_day":"2026-11-12"},"pad":"` + big + `"}`),
		"no text":                   json.RawMessage(`{"type":"admin","content":"` + big + `"}`),
		"a content that is no text": json.RawMessage(`{"type":"text","content":{"x":"` + big + `"}}`),
		"no object":                 json.RawMessage(`"` + big + `"`),
	} {
		out, ref, err := offloadText(root, payload)
		if err != nil || ref != nil || !bytes.Equal(out, payload) {
			t.Fatalf("%s: want it as it came, got %v %v", name, ref, err)
		}
	}
	if entries, _ := os.ReadDir(root); len(entries) != 0 {
		t.Fatalf("nothing is written: %v", entries)
	}
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
		" <html>":                                  "",
		string([]byte{0xA0}) + "<html>":            "text/html",
	} {
		if got := sniffActiveContent([]byte(text)); got != want {
			t.Fatalf("%q: got %q, want %q", text, got, want)
		}
	}
	root := t.TempDir()
	content := "<!doctype html>" + strings.Repeat("<p>page</p>", 7000)
	_, ref, err := offloadText(root, textPayload(t, content, nil))
	if err != nil || ref == nil || ref.Mime != "text/html" {
		t.Fatalf("an HTML text goes as text/html: %+v %v", ref, err)
	}
}

// FINDINGS A-264: the same text twice is one blob: its name is its hash, and the second promote
// finds it in place and leaves it, never replacing a blob a reader or Syncthing may be reading.
func TestTheSameTextTwiceIsOneBlob(t *testing.T) {
	root := t.TempDir()
	payload := textPayload(t, strings.Repeat("same ", 20000), nil)
	_, first, err := offloadText(root, payload)
	if err != nil {
		t.Fatalf("first: %v", err)
	}
	path := filepath.Join(root, "active", blobPrefix(first.BlobName), first.BlobName)
	before, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	_, second, err := offloadText(root, payload)
	if err != nil || second == nil || *second != *first {
		t.Fatalf("second: %+v %v", second, err)
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
// and the payload goes as it came.
func TestALinkInTheBlobTreeIsNeverFollowed(t *testing.T) {
	for _, planted := range []string{"staging", "active"} {
		t.Run(planted, func(t *testing.T) {
			root := t.TempDir()
			elsewhere := t.TempDir()
			if err := os.Symlink(elsewhere, filepath.Join(root, planted)); err != nil {
				t.Fatalf("plant: %v", err)
			}
			payload := textPayload(t, strings.Repeat("z", 70*1024), nil)
			out, ref, err := offloadText(root, payload)
			if err == nil || ref != nil || !bytes.Equal(out, payload) {
				t.Fatalf("want a failure and the payload as it came, got %v %v", ref, err)
			}
			if entries, _ := os.ReadDir(elsewhere); len(entries) != 0 {
				t.Fatalf("wrote through the link: %v", entries)
			}
		})
	}
}

// FINDINGS A-264: SendTraced sends a WF's text over the frame as a blob. Before, it refused it with
// MessageTooLargeError, and the text was never sent.
func TestATextOverTheFrameIsSentAsABlob(t *testing.T) {
	root := t.TempDir()
	saved := textOffloadBlobRoot
	textOffloadBlobRoot = root
	defer func() { textOffloadBlobRoot = saved }()
	tx := make(chan []byte, 1)
	sender := &NodeSender{uuid: "wf-uuid", fullName: "WF.demo@motherbee", tx: tx, state: &connectionState{connected: true}}
	content := strings.Repeat("a report line\n", 15000)
	msg := Message{
		Routing: Routing{Src: "wf-uuid", Dst: UnicastDestination("IO.slack.demo@motherbee"), TTL: 16, TraceID: "trace-big-text"},
		Meta:    Meta{MsgType: "user", Msg: stringPtr("REPORT")},
		Payload: textPayload(t, content, nil),
	}
	if len(msg.Payload) <= maxFrameSize {
		t.Fatalf("the text must be over the frame: %d bytes", len(msg.Payload))
	}
	if err := sender.Send(msg); err != nil {
		var tooLarge *MessageTooLargeError
		if errors.As(err, &tooLarge) {
			t.Fatalf("a text over the frame was refused instead of sent as a blob: %v", err)
		}
		t.Fatalf("send: %v", err)
	}
	frame := <-tx
	if len(frame) > 4096 {
		t.Fatalf("the frame carries the ref, not the text: %d bytes", len(frame))
	}
	var sent Message
	if err := json.Unmarshal(frame, &sent); err != nil {
		t.Fatalf("decode: %v", err)
	}
	var payload struct {
		Type       string   `json:"type"`
		Content    *string  `json:"content"`
		ContentRef *BlobRef `json:"content_ref"`
	}
	if err := json.Unmarshal(sent.Payload, &payload); err != nil || payload.Type != "text" || payload.Content != nil || payload.ContentRef == nil {
		t.Fatalf("want a content_ref: %s (%v)", sent.Payload, err)
	}
	if data := readBlob(t, root, *payload.ContentRef); string(data) != content {
		t.Fatalf("the blob holds another text")
	}
}
