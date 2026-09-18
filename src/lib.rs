pub mod console;
pub mod deployment;
pub mod engine;
pub mod host_setup;
pub mod host_supervisor;
pub mod diagnostics;

#[macro_export]
macro_rules! statusln {
    ($($arg:tt)*) => { $crate::diagnostics::line(format_args!($($arg)*)) };
}
pub mod store;
pub mod transport;
pub mod wire;
pub mod local_control;
pub mod shell_integration;
pub mod peer_auth;
mod host_shutdown;
