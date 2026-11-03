//! Experimental C ABI for a Skrin table of u64 keys and opaque byte records.
//!
//! The engine remains safe Rust. Pointer handling is confined to `abi`; the
//! ownership/staging adapter is safe Rust and commits through the production
//! engine. See `include/skrin.h` and `docs/c-api.md` for the complete contract.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![warn(missing_docs)]

mod abi;
mod store;
pub use abi::*;

type Result<T> = std::result::Result<T, Failure>;
struct Failure {
    code: i32,
    message: String,
}
impl Failure {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn argument(message: &'static str) -> Self {
        Self::new(SKRIN_INVALID_ARGUMENT, message)
    }
}
impl From<skrin::Error> for Failure {
    fn from(error: skrin::Error) -> Self {
        use skrin::Error;
        let code = match &error {
            Error::Io(_) => SKRIN_IO_ERROR,
            Error::CommitUncertain(_) => SKRIN_COMMIT_UNCERTAIN,
            Error::MaintenanceUncertain(_) => SKRIN_MAINTENANCE_UNCERTAIN,
            Error::Poisoned => SKRIN_POISONED,
            Error::Busy | Error::QueueFull => SKRIN_BUSY,
            Error::InvalidOperation(_) => SKRIN_INVALID_ARGUMENT,
            Error::DuplicateKey(_) => SKRIN_DUPLICATE_KEY,
            Error::MissingKey(_) => SKRIN_NOT_FOUND,
            Error::LimitExceeded { .. } | Error::BudgetExceeded { .. } => SKRIN_LIMIT_EXCEEDED,
            Error::Corrupt { .. } => SKRIN_CORRUPT,
            Error::UnsupportedFormat(_) => SKRIN_UNSUPPORTED_FORMAT,
            Error::SchemaMismatch { .. } => SKRIN_SCHEMA_MISMATCH,
            Error::UnsupportedPlatform => SKRIN_UNSUPPORTED_PLATFORM,
            _ => SKRIN_ENGINE_ERROR,
        };
        Self::new(code, error.to_string())
    }
}
