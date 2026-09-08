//! pve-meta editor UI.
//!
//! `model`, `grammar`, `tree`, `edit` and `request` are pure (no `web-sys`/`wasm-bindgen`)
//! and unit-tested natively (`cargo test --lib`): between them they hold the whole of the
//! page's behaviour that is not rendering — what rows exist, who has access to them, who may
//! write them, what request one edit becomes, and whether an answer still belongs to the page.
//! Everything else only compiles for `wasm32` — it pulls in Yew/pwt/proxmox-yew-comp, which
//! assume a browser — and is gated accordingly, so a native `cargo test --lib` never needs
//! to fetch or build those crates.

pub mod edit;
pub mod grammar;
pub mod lint;
pub mod model;
pub mod request;
pub mod tree;

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
