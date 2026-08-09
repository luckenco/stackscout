export class Config {
  private stored = 0;

  get value() {
    return this.stored;
  }

  set value(value: number) {
    this.stored = value;
  }

  run() {
    new_step();
  }
}

function new_step() {}
