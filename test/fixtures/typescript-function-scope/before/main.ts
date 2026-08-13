function work() {
  oldStep();
}

function oldStep() {}
function newStep() {}

export function run(flag: boolean) {
  work();
  if (flag) {
    var work = () => {};
  }
}
