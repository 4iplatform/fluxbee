package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/open-policy-agent/opa/compile"
)

// The largest policy wasm a router's OPA region takes.
const opaMaxWasmSize = 4 * 1024 * 1024

// compileRego compiles one Rego module to the wasm of one entrypoint, as the routers load it: the
// operator's policy (SY.opa.rules) and the router's baked system policy alike. The module is read
// as policy.rego from a directory holding it alone, so that name is the location the wasm records
// and the output depends on the Rego and the compiler only. It used to record the temporary
// directory's random name, so no two compiles were the same and nothing could check the baked
// policy against its Rego (FINDINGS A-179).
func compileRego(rego string, entrypoint string) ([]byte, string, int64, error) {
	tmpDir, err := os.MkdirTemp("", "opa-compile")
	if err != nil {
		return nil, "", 0, err
	}
	defer os.RemoveAll(tmpDir)
	if err := os.WriteFile(filepath.Join(tmpDir, "policy.rego"), []byte(rego), 0o644); err != nil {
		return nil, "", 0, err
	}
	start := time.Now()
	var buf bytes.Buffer
	compiler := compile.New().
		WithTarget(compile.TargetWasm).
		WithEntrypoints(entrypoint).
		WithFS(os.DirFS(tmpDir)).
		WithPaths("policy.rego").
		WithOutput(&buf)
	if err := compiler.Build(context.Background()); err != nil {
		return nil, "", 0, err
	}
	wasm := buf.Bytes()
	wasm, normErr := normalizeWasmBytes(wasm, entrypoint)
	if normErr != nil {
		return nil, "", 0, normErr
	}
	if err := validateWasm(wasm); err != nil {
		return nil, "", 0, err
	}
	if !wasmHasExport(wasm, "opa_eval") {
		return nil, "", 0, fmt.Errorf("opa wasm missing export: opa_eval")
	}
	if len(wasm) > opaMaxWasmSize {
		return nil, "", 0, fmt.Errorf("wasm too large (%d bytes)", len(wasm))
	}
	hash := sha256.Sum256(wasm)
	return wasm, "sha256:" + hex.EncodeToString(hash[:]), time.Since(start).Milliseconds(), nil
}

func normalizeWasmBytes(data []byte, entrypoint string) ([]byte, error) {
	if len(data) >= 4 && bytes.Equal(data[:4], []byte{0x1f, 0x8b, 0x08, 0x00}) {
		reader, err := gzip.NewReader(bytes.NewReader(data))
		if err != nil {
			return nil, fmt.Errorf("invalid gzip wasm: %w", err)
		}
		defer reader.Close()
		decoded, err := io.ReadAll(reader)
		if err != nil {
			return nil, fmt.Errorf("failed to decode gzip wasm: %w", err)
		}
		data = decoded
	}
	if len(data) >= 4 && bytes.Equal(data[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return data, nil
	}
	if wasm, err := extractWasmFromBundle(data, entrypoint); err == nil {
		return wasm, nil
	}
	return data, nil
}

func extractWasmFromBundle(data []byte, entrypoint string) ([]byte, error) {
	tr := tar.NewReader(bytes.NewReader(data))
	var fallback []byte
	var entrypointMatch []byte
	var policyMatch []byte
	var opaEvalMatch []byte
	debug := os.Getenv("OPA_DEBUG_BUNDLE") == "1"
	entrypoint = strings.TrimPrefix(entrypoint, "/")
	entrypoint = strings.TrimSuffix(entrypoint, "/")
	entrypointFile := ""
	if entrypoint != "" {
		parts := strings.Split(entrypoint, "/")
		entrypointFile = parts[len(parts)-1] + ".wasm"
	}
	for {
		hdr, err := tr.Next()
		if err != nil {
			if errors.Is(err, io.EOF) {
				break
			}
			return nil, err
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		if strings.HasSuffix(hdr.Name, ".wasm") {
			wasm, err := io.ReadAll(tr)
			if err != nil {
				return nil, err
			}
			hasEval := wasmHasExport(wasm, "opa_eval")
			if debug {
				log.Printf("bundle wasm entry=%s size=%d opa_eval=%v", hdr.Name, len(wasm), hasEval)
			}
			if hasEval {
				opaEvalMatch = wasm
			}
			if entrypoint != "" && strings.HasSuffix(hdr.Name, entrypoint+".wasm") {
				entrypointMatch = wasm
			} else if entrypointFile != "" && strings.HasSuffix(hdr.Name, entrypointFile) {
				entrypointMatch = wasm
			} else if strings.HasSuffix(hdr.Name, "policy.wasm") {
				policyMatch = wasm
			} else if fallback == nil {
				fallback = wasm
			}
		}
	}
	if opaEvalMatch != nil {
		if debug {
			log.Printf("bundle selected wasm by export opa_eval")
		}
		return opaEvalMatch, nil
	}
	if entrypointMatch != nil {
		if debug {
			log.Printf("bundle selected wasm by entrypoint")
		}
		return entrypointMatch, nil
	}
	if policyMatch != nil {
		if debug {
			log.Printf("bundle selected wasm policy.wasm")
		}
		return policyMatch, nil
	}
	if fallback != nil {
		if debug {
			log.Printf("bundle selected wasm fallback")
		}
		return fallback, nil
	}
	return nil, fmt.Errorf("no wasm entry found in bundle")
}

func wasmHasExport(wasm []byte, name string) bool {
	if len(wasm) < 8 {
		return false
	}
	if !bytes.Equal(wasm[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return false
	}
	off := 8
	for off < len(wasm) {
		sectionID := wasm[off]
		off++
		secLen, n := readU32LEB(wasm[off:])
		if n == 0 {
			return false
		}
		off += n
		if off+int(secLen) > len(wasm) {
			return false
		}
		if sectionID == 7 {
			sec := wasm[off : off+int(secLen)]
			count, n := readU32LEB(sec)
			if n == 0 {
				return false
			}
			cursor := n
			for i := 0; i < int(count); i++ {
				nameLen, n := readU32LEB(sec[cursor:])
				if n == 0 {
					return false
				}
				cursor += n
				if cursor+int(nameLen) > len(sec) {
					return false
				}
				expName := string(sec[cursor : cursor+int(nameLen)])
				cursor += int(nameLen)
				if cursor >= len(sec) {
					return false
				}
				cursor++ // kind
				_, n = readU32LEB(sec[cursor:])
				if n == 0 {
					return false
				}
				cursor += n
				if expName == name {
					return true
				}
			}
			return false
		}
		off += int(secLen)
	}
	return false
}

func readU32LEB(data []byte) (uint32, int) {
	var result uint32
	var shift uint
	for i := 0; i < len(data); i++ {
		b := data[i]
		result |= uint32(b&0x7f) << shift
		if b&0x80 == 0 {
			return result, i + 1
		}
		shift += 7
		if shift >= 32 {
			break
		}
	}
	return 0, 0
}

func validateWasm(wasm []byte) error {
	if len(wasm) < 8 {
		return fmt.Errorf("invalid wasm output: too short (%d bytes)", len(wasm))
	}
	if !bytes.Equal(wasm[:4], []byte{0x00, 0x61, 0x73, 0x6d}) {
		return fmt.Errorf("invalid wasm magic: %x", wasm[:4])
	}
	version := binary.LittleEndian.Uint32(wasm[4:8])
	if version != 1 {
		return fmt.Errorf("unsupported wasm version: %d", version)
	}
	return nil
}
