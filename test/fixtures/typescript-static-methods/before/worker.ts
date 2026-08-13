export class Worker {
  static run() {
    oldStatic();
  }

  run() {
    stable();
  }
}

function oldStatic() {}
function newStatic() {}
function stable() {}
