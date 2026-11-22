package lib

import (
	"io"
	"io/ioutil"
)

// Read reads r.
func Read(r io.Reader) []byte {
	b, _ := ioutil.ReadAll(r) // OLD:go-ioutil-io-os:modern
	return b
}
