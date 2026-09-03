// Entity extraction fixture for Swift: covers all 14 required kinds.
import Foundation

// Add adds two integers (public top-level function -> Function + Export).
public func add(a: Int, b: Int) -> Int {
    return a + b
}

// User is a public class -> Class + Export.
public class User {
    let name: String
    var age: Int

    init(name: String, age: Int) {
        self.name = name
        self.age = age
    }

    func greet() -> String {
        return "hello " + name
    }
}

// Greeter is a protocol (Swift's abstraction mechanism) -> Interface.
protocol Greeter {
    func greet() -> String
}

// Top-level variables -> Variable.
let version = "1.0.0"
var flag = true

// GreeterImpl conforms to Greeter.
class GreeterImpl: Greeter {
    func greet() -> String {
        return "hi"
    }
}

func main() {
    let app = Application()
    // Route: Vapor-style app.get("/users") { ... }.
    app.get("/users") { req in
        let res = Response()
        // Response: res.send(...).
        res.send("ok")
    }
    app.run()
}

func compute(x: String, n: Int) -> String {
    let names = ["a", "b"]
    for name in names {
        if name.count > 0 {
            throw RuntimeError("bad name")
        }
    }
    var i = 0
    while i < n {
        i += 1
    }
    do {
        let result = try risky()
        return result
    } catch let err {
        return "caught"
    }
}

// Point is a struct (also class_declaration in this grammar).
struct Point {
    let x: Int
    let y: Int
}
