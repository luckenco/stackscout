export function replace() {
  stableMissing();
  newMissing();
}

export function syntaxOnly() {
  stableMissing();
  return 2;
}

export function reorder() {
  secondMissing();
  firstMissing();
}
