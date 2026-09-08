//! The listening-test data model: sessions, their blinding, results and region
//! annotations.
//!
//! No window and no audio device — the shapes and the rules, so the app and the
//! command line agree about what a session is.

pub mod peaks;
pub mod store;
pub mod types;

pub use peaks::{Column, Peaks};
pub use store::{Store, StoreError, annotations_file_for, empty_annotations};
pub use types::*;
