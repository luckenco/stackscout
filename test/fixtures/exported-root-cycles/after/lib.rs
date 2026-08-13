pub fn alpha() {
    beta();
    changed();
}

pub fn beta() {
    alpha();
}

pub fn delta() {
    echo();
    changed();
}

pub fn echo() {
    delta();
}

fn changed() {
    let _ = 2;
}
