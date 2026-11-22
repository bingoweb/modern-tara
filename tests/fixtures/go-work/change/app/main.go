package main

import (
	"strings"

	"example.com/lib"
)

func main() { _ = lib.Read(nil) }

func show(v interface{}) {} // OLD:go/any:modern

func parts(s string) (n int) {
	for _, p := range strings.Split(s, ",") { // OLD:go/stringsseq:needs-newer
		n += len(p)
	}
	return n
}
