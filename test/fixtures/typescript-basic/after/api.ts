export function run() {
  keep();
  added();
}

function keep() {}
function added() {}

export class Service {
  execute() {
    this.keep();
    this.added();
  }

  keep() {}
  added() {}
}
