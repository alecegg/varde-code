// Expression fixtures: Call (call_expression), MemberAccess (field_expression),
// Literal (integer/float/string/raw-string/char/boolean).

fn area(p: &Point) -> f64 {
    let w: f64 = 2.5;
    let s = "str";
    let raw = r#"raw text"#;
    let c = 'x';
    let b = true;
    let n = false;

    let dx = p.x;
    let value = compute(1, 2.0);
    let total = w * dx;
    value + total
}
