pub fn handle(status: i32) {
    if status == 1 {
        do_a();
    } else if status == 2 {
        do_b();
    } else {
        do_other();
    }

    match status {
        3 => do_three(),
        4 if guard() => do_guarded(),
        _ => do_default(),
    }
}

fn guard() -> bool {
    old_guard_step();
    true
}

fn do_a() {}
fn do_b() {}
fn do_other() {}
fn do_three() {}
fn do_guarded() {}
fn do_default() {}
fn old_guard_step() {}
