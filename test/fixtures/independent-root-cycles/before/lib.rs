fn alpha() {
    beta();
    let _ = 1;
}

fn beta() {
    alpha();
    let _ = 1;
}

fn delta() {
    echo();
    let _ = 1;
}

fn echo() {
    delta();
    let _ = 1;
}
