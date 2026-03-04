//! Safe Rust bindings for PJSIP's PJSUA library.

mod ffi;
mod ffi_helpers;
mod error;
pub mod types;
pub mod version;

pub use error::{check_status, PjError, Result};
pub use ffi_helpers::{pj_str_to_string, PjString};
pub use types::*;
