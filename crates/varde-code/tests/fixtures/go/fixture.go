package main

import (
	"fmt"
	"net/http"
)

// Add adds two integers (exported).
func Add(a int, b int) int {
	return a + b
}

// User is an exported struct type.
type User struct {
	Name string
}

// Greeter is an exported interface type.
type Greeter interface {
	Greet() string
}

var version = "1.0.0"

const maxRetries = 3

func main() {
	app := http.NewServeMux()
	app.HandleFunc("/users", handler)
	resp := compute(version, maxRetries)
	fmt.Println(resp)
	go worker()
}

func worker() {}

func compute(x string, n int) string {
	names := []string{"a", "b"}
	for i, name := range names {
		if len(name) > 0 {
			panic("bad name")
		}
	}
	for i := 0; i < n; i++ {
		if i == 2 {
			break
		}
		continue
	}
	switch n {
	case 1:
		return x
	default:
		return "other"
	}
	defer func() {
		if r := recover(); r != nil {
			fmt.Println("recovered", r)
		}
	}()
	defer fmt.Println("done")
	return x
}

func handler(w http.ResponseWriter, r *http.Request) {
	w.WriteHeader(200)
	w.Write([]byte("ok"))
}

func (u *User) Greet() string {
	return u.Name
}
