package main

import "fmt"

func old(v interface{}) string { return fmt.Sprint(v) } // UNCHANGED

func main() {
	fmt.Println(old(1))
}
