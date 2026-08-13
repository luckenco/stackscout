fn alpha() {
    beta();
    let _ = 2;
}

fn beta() {
    alpha();
    let _ = 2;
}

fn delta() {
    echo();
    let _ = 2;
}

fn echo() {
    delta();
    let _ = 2;
}
