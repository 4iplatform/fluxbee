//go:build !linux
// +build !linux

package main

func main() {
	panic("sy-opa-rules supports only Linux targets.")
}
