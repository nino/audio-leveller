//! Loading the three graphs and running spans of frames through them.
//!
//! Inference is `tract`, which is pure Rust. That is not the fastest option —
//! ONNX Runtime generally is — but it costs no native library, which means
//! nothing to download at build time, nothing to sign, nothing to make
//! universal, and nothing extra inside the `.app`. Upstream DeepFilterNet runs
//! this same export through tract, so it is a road already driven.
//!
//! ## Inputs by name, outputs by position
//!
//! The export names its inputs — `feat_erb`, `emb`, `e3` — and those names are
//! meaningful, so they are what this matches on. It does not name its outputs
//! usefully: what survives into the graph is an internal node name like
//! `/erb_conv0/3/Relu`, so outputs have to be taken positionally. A reordered
//! export would then be read wrongly and silently, which is why every output's
//! shape is checked against what the configuration says it should be. That
//! turns a silent misread into a refusal to load.

use std::path::Path;

use tract_onnx::prelude::*;

use crate::deepfilternet::{Config, ModelOutput};
use crate::registry::{ModelSpec, Role, model_file_path, verify_model};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0}")]
    Weights(String),
    #[error("loading {file}: {source}")]
    Load {
        file: String,
        #[source]
        source: TractError,
    },
    #[error("running {graph}: {source}")]
    Run {
        graph: &'static str,
        #[source]
        source: TractError,
    },
    #[error(
        "{graph} output {index} has shape {got:?}, expected {want:?} — this is not the export \
         these constants were derived from"
    )]
    Shape {
        graph: &'static str,
        index: usize,
        got: Vec<usize>,
        want: Vec<usize>,
    },
}

type Runnable = std::sync::Arc<TypedRunnableModel>;

/// The three graphs, loaded and optimised.
///
/// Loading is the expensive part — several seconds of graph optimisation — so a
/// session is built once and reused for every span of every channel.
pub struct Session {
    encoder: Runnable,
    erb_decoder: Runnable,
    df_decoder: Runnable,
    config: Config,
}

fn load(path: &Path) -> Result<Runnable, ModelError> {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let build = || -> TractResult<Runnable> {
        // The time axis is already symbolic in the export, so one runnable
        // model serves every span length and nothing is rebuilt per chunk.
        tract_onnx::onnx()
            .model_for_path(path)?
            .into_optimized()?
            .into_runnable()
    };
    build().map_err(|source| ModelError::Load { file, source })
}

impl Session {
    /// Load a model's graphs, refusing anything that does not verify.
    pub fn load(spec: &ModelSpec, config: Config) -> Result<Self, ModelError> {
        verify_model(spec).map_err(ModelError::Weights)?;
        let path = |role| {
            model_file_path(spec, role)
                .ok_or_else(|| ModelError::Weights(format!("{}: no path for {role:?}", spec.id)))
        };
        Ok(Self {
            encoder: load(&path(Role::Encoder)?)?,
            erb_decoder: load(&path(Role::ErbDecoder)?)?,
            df_decoder: load(&path(Role::DfDecoder)?)?,
            config,
        })
    }

    /// Run the three graphs over one span of frames.
    pub fn run_span(
        &self,
        erb: &[f32],
        spec: &[f32],
        frames: usize,
    ) -> Result<ModelOutput, ModelError> {
        let (nb_erb, nb_df, df_order) = (
            self.config.nb_erb,
            self.config.nb_df,
            self.config.df_order,
        );

        let feat_erb = tensor("encoder", &[1, 1, frames, nb_erb], erb)?;
        let feat_spec = tensor("encoder", &[1, 2, frames, nb_df], spec)?;
        let encoded = self
            .encoder
            .run(tvec![feat_erb.into(), feat_spec.into()])
            .map_err(|source| ModelError::Run {
                graph: "encoder",
                source,
            })?;

        // e0, e1, e2, e3, emb, c0, lsnr — checked rather than assumed, because
        // the names that would have said so do not survive the export.
        check("encoder", &encoded, 0, &[1, 64, frames, 32])?;
        check("encoder", &encoded, 1, &[1, 64, frames, 16])?;
        check("encoder", &encoded, 2, &[1, 64, frames, 8])?;
        check("encoder", &encoded, 3, &[1, 64, frames, 8])?;
        check("encoder", &encoded, 4, &[1, frames, 512])?;
        check("encoder", &encoded, 5, &[1, 64, frames, nb_df])?;
        check("encoder", &encoded, 6, &[1, frames, 1])?;

        // The ERB decoder takes them back in the order emb, e3, e2, e1, e0.
        let mask = self
            .erb_decoder
            .run(tvec![
                encoded[4].clone(),
                encoded[3].clone(),
                encoded[2].clone(),
                encoded[1].clone(),
                encoded[0].clone(),
            ])
            .map_err(|source| ModelError::Run {
                graph: "erb decoder",
                source,
            })?;
        check("erb decoder", &mask, 0, &[1, 1, frames, nb_erb])?;

        let filtered = self
            .df_decoder
            .run(tvec![encoded[4].clone(), encoded[5].clone()])
            .map_err(|source| ModelError::Run {
                graph: "df decoder",
                source,
            })?;
        check(
            "df decoder",
            &filtered,
            0,
            &[1, frames, nb_df, df_order * 2],
        )?;

        Ok(ModelOutput {
            gains: values("erb decoder", &mask[0])?,
            coefs: values("df decoder", &filtered[0])?,
            lsnr: values("encoder", &encoded[6])?,
        })
    }
}

fn tensor(graph: &'static str, shape: &[usize], data: &[f32]) -> Result<Tensor, ModelError> {
    Tensor::from_shape(shape, data).map_err(|source| ModelError::Run { graph, source })
}

fn check(
    graph: &'static str,
    outputs: &[TValue],
    index: usize,
    want: &[usize],
) -> Result<(), ModelError> {
    let got = outputs
        .get(index)
        .map(|t| t.shape().to_vec())
        .unwrap_or_default();
    if got != want {
        return Err(ModelError::Shape {
            graph,
            index,
            got,
            want: want.to_vec(),
        });
    }
    Ok(())
}

fn values(graph: &'static str, tensor: &TValue) -> Result<Vec<f32>, ModelError> {
    Ok(tensor
        .to_plain_array_view::<f32>()
        .map_err(|source| ModelError::Run { graph, source })?
        .iter()
        .copied()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DFN3;
    use crate::registry::MODELS;

    /// The weights are not in the repository, so every test here is skipped
    /// when they are absent — loudly enough that a run with none of them
    /// cannot be mistaken for a run that checked something.
    fn session() -> Option<Session> {
        let spec = &MODELS[0];
        if let Err(reason) = verify_model(spec) {
            eprintln!("skipping: {reason}");
            return None;
        }
        Some(Session::load(spec, DFN3).expect("verified weights should load"))
    }

    #[test]
    fn the_graphs_load_and_produce_the_shapes_the_dsp_expects() {
        let Some(session) = session() else { return };
        let frames = 12;
        let erb = vec![0.0f32; frames * DFN3.nb_erb];
        let spec = vec![0.0f32; 2 * frames * DFN3.nb_df];

        let out = session.run_span(&erb, &spec, frames).expect("a span runs");
        assert_eq!(out.gains.len(), frames * DFN3.nb_erb);
        assert_eq!(out.coefs.len(), frames * DFN3.nb_df * DFN3.df_order * 2);
        assert_eq!(out.lsnr.len(), frames);
        assert!(out.gains.iter().all(|v| v.is_finite()));
        assert!(out.lsnr.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn the_mask_is_a_gain_between_zero_and_one() {
        // The ERB decoder ends in a sigmoid, so anything outside this range
        // means the output was read from the wrong graph output.
        let Some(session) = session() else { return };
        let frames = 12;
        let out = session
            .run_span(
                &vec![0.0f32; frames * DFN3.nb_erb],
                &vec![0.0f32; 2 * frames * DFN3.nb_df],
                frames,
            )
            .expect("a span runs");
        assert!(
            out.gains.iter().all(|g| (0.0..=1.0).contains(g)),
            "a gain outside [0, 1] is not a mask"
        );
    }

    #[test]
    fn a_span_of_a_different_length_runs_without_rebuilding() {
        // The time axis is symbolic in the export, which is what lets one
        // session serve the full chunks and the short last one.
        let Some(session) = session() else { return };
        for frames in [4usize, 37, 100] {
            let out = session
                .run_span(
                    &vec![0.0f32; frames * DFN3.nb_erb],
                    &vec![0.0f32; 2 * frames * DFN3.nb_df],
                    frames,
                )
                .unwrap_or_else(|e| panic!("{frames} frames: {e}"));
            assert_eq!(out.lsnr.len(), frames);
        }
    }
}
