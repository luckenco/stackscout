use crate::left::{Worker, work};

mod left {
    pub struct Worker;
    impl Worker { pub fn new() { audit(); } }
    pub fn work() { audit(); }
    fn audit() {}
}
mod right {
    pub struct Worker;
    impl Worker { pub fn new() { audit(); } }
    fn audit() {}
}
pub fn outer() {
    Worker::new();
    { use crate::right::Worker; Worker::new(); }
    { use external::Worker; Worker::new(); }
    Worker::new();
    work();
}
pub fn parameter(work: fn()) { work(); }
pub fn local() { let work = factory(); work(); }
pub fn receiver(worker: Foreign) { worker.work(); }
pub fn if_let(value: Option<fn()>) { if let Some(work) = value { work(); } }
pub fn for_loop(values: Vec<fn()>) { for work in values { work(); } }
pub fn match_arm(value: Option<fn()>) { match value { Some(work) => work(), None => {} } }
pub fn local_type() { struct Worker; Worker::new(); }
pub fn generic<Worker>() { Worker::new(); }
struct Factory<T>(T);
impl<Worker> Factory<Worker> { pub fn generic_impl() { Worker::new(); } }
pub fn constructor(value: Option<i32>) { if let Some(v) = value { Some(v); } }
pub mod relative {
    use left::Worker;
    mod left {
        pub struct Worker;
        impl Worker { pub fn new() { audit(); } }
        fn audit() {}
    }
    pub fn run() { Worker::new(); }
}
pub fn wildcard() { use crate::right::*; Worker::new(); }
use crate::left as worker;
mod nested {
    use super::right::{self, Worker};
    pub fn inner() { Worker::new(); right::Worker::new(); }
    pub fn no_parent_import() { WorkerNotImported::new(); }
    mod deeper {
        use super::super::left::Worker;
        pub fn run() { Worker::new(); }
    }
}
mod sibling {
    pub fn no_parent_import() { Worker::new(); work(); }
}
