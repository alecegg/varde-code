fn helper_one(x: i32) -> i32 {
    let y = x * 2;
    let z = y + 1;
    z * 3
}

fn helper_two(x: i32) -> i32 {
    let y = x * 2;
    let z = y + 1;
    z * 3
}

fn unique_thing(s: String) -> String {
    s.trim().to_uppercase()
}
