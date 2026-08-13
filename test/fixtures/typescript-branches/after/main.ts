export function handle(status: string) {
  if (status === "a") {
    do_a();
  } else if (status === "b") {
    do_b();
    do_extra();
  } else {
    do_other();
  }
}

function do_a() {}
function do_b() {}
function do_extra() {}
function do_other() {}
