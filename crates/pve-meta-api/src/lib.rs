//! `pve-meta-api`: `#[api]` handlers, router tree, auth and shared types for
//! `pve-metad` (the HTTPS daemon) and `pve-meta` (the CLI, which runs the same
//! handlers in-process against the store, no HTTP involved).

pub mod api;
pub mod auth;
pub mod error;
pub mod store;
