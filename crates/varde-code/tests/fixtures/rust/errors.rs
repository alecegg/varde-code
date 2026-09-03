// Error fixtures: Rust has no try/catch; `panic!()` is its idiomatic
// equivalent and maps to Throw (named after the panic message argument).

fn load(path: &str) -> Result<String, String> {
    if path.is_empty() {
        panic!("empty path: {path}");
    }
    let content = read_file(path);
    if content.is_empty() {
        return Err("no content".to_string());
    }
    Ok(content)
}

fn read_file(path: &str) -> String {
    String::new()
}
