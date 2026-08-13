pub fn rust_outer() {
    let callback = || hidden_rust();
    visible_rust();
}

fn hidden_rust() {}
fn hidden_rust_other() {}
fn visible_rust() {}
fn visible_rust_extra() {}
