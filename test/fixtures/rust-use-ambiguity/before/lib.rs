#[cfg(feature = "left")]
use crate::left::Worker;
#[cfg(not(feature = "left"))]
use crate::right::Worker;
mod left {
    pub struct Worker;
    impl Worker { pub fn new() {} }
    fn audit() {}
}
mod right {
    pub struct Worker;
    impl Worker { pub fn new() {} }
}
pub fn run() { Worker::new(); }
mod unknown {
    #[cfg(feature = "left")]
    use crate::left::Worker;
    #[cfg(not(feature = "left"))]
    use external::Worker;
    pub fn run() {}
}
