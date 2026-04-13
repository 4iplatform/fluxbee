//go:build !linux

package sdk

import "fmt"

func openRouterSHMReadOnly(name string) ([]byte, func() error, error) {
	return nil, func() error { return nil }, fmt.Errorf("router shm resolution is only supported on linux")
}
