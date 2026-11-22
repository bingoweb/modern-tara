package main

import "fmt"

func old(v interface{}) string { return fmt.Sprint(v) } // UNCHANGED

func main() {
	fmt.Println(old(1), describe(2), count("a,b"), len(sorted(nil)))
	loop()
}
