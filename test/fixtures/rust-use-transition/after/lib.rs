use crate::right::Worker;

mod left {
    pub struct Worker;
    impl Worker { pub fn new() {} }
}
mod right {
    pub struct Worker;
    impl Worker { pub fn new() {} }
}
pub fn run() { Worker::new(); }
