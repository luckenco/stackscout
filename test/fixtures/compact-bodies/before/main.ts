export function old() { stable(); stable(); changed(); }
function stable() { if (ready) { leaf(); missing(); } }
function leaf() {}
function changed() { bridge(); }
function bridge() { oldMissing(); }
function audit() {}
