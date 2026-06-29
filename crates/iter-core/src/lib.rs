//! Shared primitives for the Iter Maps backend services.

pub mod config;
pub mod error;
pub mod health;
pub mod reliability;
pub mod shutdown;
pub mod telemetry;

pub use error::{ApiError, code};
pub use health::Status;
