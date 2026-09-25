pub mod commands;
pub mod data;
pub mod icon;
pub mod menu;
pub mod notify;
#[cfg(test)]
mod sysdeps;
pub mod tray;
pub mod view;
#[cfg(target_os = "linux")]
pub mod windows;

pub use commands::TrayCommand;
pub use data::{DataSource, Query};
pub use tray::TrayApp;
