function work() {
  oldStep();
}

function oldStep() {}
function newStep() {}

export function run() {
  work();
  {
    const work = () => {};
  }
}
