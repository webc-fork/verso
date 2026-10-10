//! Verso Web Browser
//!
//! This is the documentation of Verso's types and items.
//! See [GitHub repository](https://github.com/versotile-org/verso) for more general introduction.

#![deny(missing_docs)]

/// Utilities to read options and preferences.
pub mod config;
/// Error and result types.
pub mod errors;

pub use errors::{Error, Result};
/// Re-exporting Winit for the sake of convenience.
pub use winit;
