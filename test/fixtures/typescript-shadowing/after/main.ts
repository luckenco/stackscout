function work() {
  return 2;
}

export function parameter(work: () => number) {
  return work();
}

export function local() {
  const work = () => 3;
  return work();
}

export function make(Worker: new () => unknown) {
  return new Worker();
}

class Worker {
  constructor() {
    oldStep();
    newStep();
  }
}

function oldStep() {}
function newStep() {}
