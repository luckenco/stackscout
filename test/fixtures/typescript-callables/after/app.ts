export const boot = () => {
  load();
  migrate();
};

export const named = function () {
  stable();
  named_extra();
};

export default function () {
  stable();
  audit();
}

export function* stream() {
  yield load();
  yield stream_extra();
}

export function make() {
  new Worker();
}

export class Worker {
  constructor() {
    init();
    ready();
  }

  helper = () => {
    work();
    helper_extra();
  };

  start() {
    this.helper();
  }
}

function load() {}
function migrate() {}
function stable() {}
function named_extra() {}
function audit() {}
function stream_extra() {}
function init() {}
function ready() {}
function work() {}
function helper_extra() {}
