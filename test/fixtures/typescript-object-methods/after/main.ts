const first = {
  run() {
    new_anonymous();
  },
};

const second = {
  run() {
    stable_anonymous();
  },
};

export function entry() {
  new_step();
}

function new_step() {}
