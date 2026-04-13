//go:build linux

package sdk

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

func openRouterSHMReadOnly(name string) ([]byte, func() error, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return nil, nil, fmt.Errorf("router shm name must be non-empty")
	}
	path := filepath.Join("/dev/shm", strings.TrimPrefix(name, "/"))
	file, err := os.Open(path)
	if err != nil {
		return nil, nil, err
	}
	info, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, nil, err
	}
	size := int(info.Size())
	if size <= 0 {
		_ = file.Close()
		return nil, nil, fmt.Errorf("router shm %s is empty", name)
	}
	data, err := syscall.Mmap(int(file.Fd()), 0, size, syscall.PROT_READ, syscall.MAP_SHARED)
	if err != nil {
		_ = file.Close()
		return nil, nil, err
	}
	closeFn := func() error {
		var firstErr error
		if err := syscall.Munmap(data); err != nil {
			firstErr = err
		}
		if err := file.Close(); err != nil && firstErr == nil {
			firstErr = err
		}
		return firstErr
	}
	return data, closeFn, nil
}
