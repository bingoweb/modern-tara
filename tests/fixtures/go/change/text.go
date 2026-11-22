package main

import (
	"fmt"
	"io"
	"io/ioutil"
	"math/rand" // OLD:go-math-rand-v2:needs-newer
	"net"
	"reflect"
	"sort"
	"strings"
)

func describe(v interface{}) string { // OLD:go/any:modern
	return fmt.Sprint(v)
}

func keep(v any) any { return v } // DECOY

func count(s string) int {
	n := 0
	for _, part := range strings.Split(s, ",") { // OLD:go/stringsseq:needs-newer
		n += len(part)
	}
	return n
}

func loop() {
	for i := 0; i < 3; i++ { // OLD:go/rangeint:needs-newer
		fmt.Println(i, rand.Intn(3))
	}
}

func read(r io.Reader) []byte {
	b, _ := ioutil.ReadAll(r) // OLD:go-ioutil-io-os:modern
	return b
}

func pointer() reflect.Type {
	return reflect.PtrTo(reflect.ValueOf(0).Type()) // OLD:go-reflect-ptr-pointer:modern
}

func sorted(names []string) []string {
	sort.Strings(names) // OLD:go-sort-basic-slices-sort:modern
	return names
}

var sink io.Writer = ioutil.Discard // OLD:go-ioutil-discard:modern

type inner struct{ A int }

type record struct {
	In inner `json:"in,omitempty"` // OLD:go/omitzero:needs-newer
}

// hostport is a correctness check, switched off: no finding here.
func dial(host string, port int) (net.Conn, error) { return net.Dial("tcp", fmt.Sprintf("%s:%d", host, port)) } // DECOY

// The inliner refuses a non-std callee declared with go1.24: needs-newer, never modern.
func useOld() int { return oldName() } // OLD:go/inline:needs-newer

// A low catalogue rule on the same line never drops the go/inline finding.
func listOld() { _, _ = ioutil.ReadDir(oldDir(".")) } // OLD:go/inline:needs-newer
