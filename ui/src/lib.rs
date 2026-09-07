//! pve-meta editor UI.
//!
//! `model` and `request` are pure (no `web-sys`/`wasm-bindgen`) and unit-tested natively
//! (`cargo test --lib`). Everything else only compiles for `wasm32` — it pulls in
//! Yew/pwt/proxmox-yew-comp, which assume a browser — and is gated accordingly, so a
//! native `cargo test --lib` never needs to fetch or build those crates.

pub mod model;
pub mod request;

#[cfg(target_arch = "wasm32")]
pub mod api;
#[cfg(target_arch = "wasm32")]
pub mod app;
#[cfg(target_arch = "wasm32")]
pub mod auth;
#[cfg(target_arch = "wasm32")]
pub mod editor;
#[cfg(target_arch = "wasm32")]
pub mod monaco;
#[cfg(target_arch = "wasm32")]
pub mod theme;
