// Control-flow fixtures: if, match, while, loop, for, return, break, continue.
// (`if let` / `while let` parse as if_expression / while_expression with a
// let_condition — there are no separate node kinds in tree-sitter-rust.)

fn classify(n: i32) -> i32 {
    if n > 0 {
        return 1;
    }

    match n {
        0 => 0,
        _ => -1,
    }
}

fn spin(limit: usize) {
    let mut i = 0;
    while i < limit {
        i += 1;
        continue;
    }

    loop {
        break;
    }

    for item in 0..limit {
        if item % 2 == 0 {
            continue;
        }
        process(item);
    }
}
