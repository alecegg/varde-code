// Structural fixtures: Function, Class (struct + enum), Interface (trait),
// Parameter. Rust has no native try/catch construct and no built-in routing —
// Catch/Route/Response are carved out (see REQUIRED_KINDS in
// src/extract/langs/rust.rs).

struct Point {
    x: i32,
    y: i32,
}

enum Color {
    Red,
    Green,
}

trait Shape {
    fn area(&self) -> f64;
}

fn greet(name: &str) -> String {
    format!("Hello {name}")
}

impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

impl Shape for Point {
    fn area(&self) -> f64 {
        let Point { x, y } = *self;
        (x * x + y * y) as f64
    }
}
