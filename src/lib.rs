//! Rustle — a minimalist self-hosted RSS reader.
//!
//! Everything lives in the library so that the integration tests in `tests/` can build and
//! drive a real router; `src/main.rs` is only startup wiring.

pub mod assets;
pub mod config;
pub mod db;

pub mod error;
pub mod feed;
pub mod password;
pub mod state;
pub mod theme;
pub mod web;

pub use config::Config;
pub use error::AppError;
pub use state::AppState;
pub use theme::Theme;
pub use web::build_router;
