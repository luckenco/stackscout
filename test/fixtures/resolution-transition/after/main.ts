import { task as choose } from "./a";
import { task as choose } from "./b";
import { ready } from "./dep";

export function run() {
  choose();
  ready();
}
