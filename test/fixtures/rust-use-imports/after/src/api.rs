use crate::{worker::{self as service, Worker as Builder, save as persist}};
use crate::worker::Worker;

pub fn grouped() { service::Worker::new(); }
pub fn direct() { Worker::new(); }
pub fn alias() { Builder::new(); }
pub fn function() { persist(); }
