const first = {
  run() {
    old_anonymous();
  },
};

const second = {
  run() {
    stable_anonymous();
  },
};

export function entry() {
  old_step();
}

function old_step() {}
