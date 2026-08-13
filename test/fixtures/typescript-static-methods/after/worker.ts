export class Worker {
  static run() {
    newStatic();
  }

  run() {
    stable();
  }
}

function oldStatic() {}
function newStatic() {}
function stable() {}
