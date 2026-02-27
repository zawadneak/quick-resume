pub mod attach;
pub mod info;
pub mod suspend;

pub use attach::{open_process_by_name, ProcessHandle};
pub use suspend::SuspendGuard;
