package main

import (
	"fmt"

	"example.com/app"
	"example.com/app/sub"
)

func main() {
	// Imports the module-root package and a subpackage; `fmt` is stdlib and
	// stays unresolved.
	fmt.Println(app.Root(), sub.Help())
}
