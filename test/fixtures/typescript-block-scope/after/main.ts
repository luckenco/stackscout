function work() {
  newStep();
}

function oldStep() {}
function newStep() {}

export function run() {
  work();
  {
    const work = () => {};
  }
}
