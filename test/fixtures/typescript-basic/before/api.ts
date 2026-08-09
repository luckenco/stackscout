export function run() {
  keep();
  removed();
}

function keep() {}
function removed() {}

export class Service {
  execute() {
    this.keep();
    this.removed();
  }

  keep() {}
  removed() {}
}
