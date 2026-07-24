fn alpha() {
    let x = 1;
    let y = 2;
    let z = x + y;
    consume(z);
}

fn beta(a: i32) -> i32 {
    a * 3
}

fn gamma() -> bool {
    true
}

fn consume(value: i32) {
    drop(value);
}
