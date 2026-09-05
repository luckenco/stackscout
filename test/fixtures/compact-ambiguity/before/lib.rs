#[cfg(left)] use crate::left::work;
#[cfg(right)] use crate::right::work;
pub fn run() {}
mod left {
    pub fn work() { sameMissing(); }
}
mod right {
    pub fn work() { nested(); }
    fn nested() { work(); }
}
