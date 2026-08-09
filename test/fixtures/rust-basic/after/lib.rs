pub fn run() {
    stable();
    added();
    tools::helper();
}

fn stable() {}
fn added() {}

pub struct Worker;

impl Worker {
    pub fn execute(&self) {
        self.added();
        Self::open();
    }

    fn added(&self) {}
    fn open() {}
}

mod tools {
    pub fn helper() {}
}
