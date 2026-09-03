// Variable fixtures: let declarations, one Variable per pattern identifier
// (plain, typed, tuple, struct-destructuring).

fn setup() {
    let x: i32 = 5;
    let y = 10;
    let (a, b) = (1, 2);
    let mut counter = 0;
    counter += 1;

    let origin = Point { x: 0, y: 0 };
    let Point { x, y } = origin;
    let message = "hello";
}
