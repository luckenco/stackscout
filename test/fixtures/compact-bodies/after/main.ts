export function first() { stable(); stable(); changed(); }
export function second() { if (ready) { stable(); changed(); } }
function stable() { if (ready) { leaf(); missing(); } }
function leaf() {}
function changed() { bridge(); }
function bridge() { newMissing(); audit(); }
function audit() {}
