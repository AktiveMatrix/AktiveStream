//! Platform-neutral client logic shared by every AktiveStream client (Windows, Linux, future
//! macOS): the connection phone book (`config`) and the SSH tunnel (`tunnel`). Features that are
//! not tied to a platform live here so all clients stay in parity instead of drifting.

pub mod bar;
pub mod capture;
pub mod config;
pub mod draw;
pub mod manager;
pub mod stats;
pub mod tunnel;
