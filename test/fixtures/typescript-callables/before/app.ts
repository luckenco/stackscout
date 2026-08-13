export const boot = () => {
  load();
};

export const named = function () {
  stable();
};

export default function () {
  stable();
}

export function* stream() {
  yield load();
}

export function make() {
  new Worker();
}

export class Worker {
  constructor() {
    init();
  }

  helper = () => {
    work();
  };

  start() {
    this.helper();
  }
}

function load() {}
function stable() {}
function init() {}
function work() {}
