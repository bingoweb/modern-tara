//go:build linux

package board

// Show is built on Linux only: the host pass on Windows or macOS skips it.
func Show(v interface{}) interface{} { return v } // OLD:go/any:modern
