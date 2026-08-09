pub fn run() {
    stable();
    removed();
    tools::helper();
}

fn stable() {}
fn removed() {}

pub struct Worker;

impl Worker {
    pub fn execute(&self) {
        self.stable();
        Self::open();
    }

    fn stable(&self) {}
    fn open() {}
}

mod tools {
    pub fn helper() {}
}
