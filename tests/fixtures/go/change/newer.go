//go:build go1.24

package main

// oldName is migrated by go fix once the caller's file reaches go 1.24.
//
//go:fix inline
func oldName() int { return newName() }

func newName() int { return 1 }

//go:fix inline
func oldDir(d string) string { return newName2(d) }

func newName2(d string) string { return d }
