function work() {
  return 1;
}

export function parameter(work: () => number) {
  return work();
}

export function local() {
  const work = () => 3;
  return work();
}
