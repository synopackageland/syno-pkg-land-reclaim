//! The DSM system partition (`/`, md0): what fills it, and the fixed,
//! conservative allowlist of what `rootfs plan` may move off it.

#[cfg(target_os = "linux")]
pub mod apply;
#[cfg(target_os = "linux")]
pub mod apply_doc;
pub mod classify;
#[cfg(target_os = "linux")]
pub mod delegate;
#[cfg(target_os = "linux")]
pub mod history;
#[cfg(target_os = "linux")]
pub mod live;
#[cfg(target_os = "linux")]
pub mod plan;
#[cfg(target_os = "linux")]
pub mod plan_doc;
#[cfg(target_os = "linux")]
pub mod report;
#[cfg(target_os = "linux")]
pub mod report_cache;
#[cfg(target_os = "linux")]
pub mod update_guard;
#[cfg(target_os = "linux")]
pub mod walk;
