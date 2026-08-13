export function replace() {
  stableMissing();
  oldMissing();
}

export function syntaxOnly() {
  stableMissing();
  return 1;
}

export function reorder() {
  firstMissing();
  secondMissing();
}
