//! DeepFilterNet3 speech enhancement.
//!
//! Split in two, deliberately. [`deepfilternet`] is the signal processing
//! around the neural graphs — the transform, the ERB filterbank, the feature
//! normalisation, the mask application, the deep filter and the resynthesis —
//! and it has no ONNX dependency at all, so it is tested without weights
//! present. Everything that needs the runtime lives beside it.

pub mod backend;
pub mod deepfilternet;
pub mod registry;
pub mod session;

pub use deepfilternet::{Config, DFN3};
pub use registry::{MODELS, ModelSpec, Role, Sha256, model_directory, model_for, sha256, sha256_file, verify_model};
pub use backend::{Model, backends, registry, weights_installed};
pub use session::{ModelError, Session};
