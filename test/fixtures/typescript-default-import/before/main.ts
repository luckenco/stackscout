import work from "./dep.js";
import Worker from "./worker.js";

export function run() {
  work();
}

export function make() {
  return new Worker();
}
