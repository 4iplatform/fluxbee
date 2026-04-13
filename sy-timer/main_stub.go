//go:build !linux
// +build !linux

package main

func main() {
	panic("sy-timer supports only Linux targets.")
}
