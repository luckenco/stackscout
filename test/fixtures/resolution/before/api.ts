import { task as choose } from "./a";
import { task as choose } from "./b";

export function run() {
  missing();
  choose();
  stable();
  return 1;
}

function stable() {}
