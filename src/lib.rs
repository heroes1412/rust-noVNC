//! A stand-alone replacement for noVNC's `utils/novnc_proxy` plus
//! `websockify`.
//!
//! The noVNC client itself is plain JavaScript and runs unmodified in the
//! browser. The only piece that historically needed Python was the small
//! helper that (a) serves the static files and (b) bridges WebSockets to a
//! raw TCP VNC server. This crate implements both, so a single self-contained
//! executable is enough on Windows, macOS and Linux.
//!
//! Two front-ends are provided:
//! * [`run`] -- start the server from a [`Config`]
//! * [`Args`] -- the command line interface (mirrors `novnc_proxy`)

pub mod config;
pub mod record;
pub mod server;
pub mod tls;
pub mod web;
pub mod ws;

pub use config::{Args, Config};
pub use server::{run, run_on};

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
