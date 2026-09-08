//! Which weights exist, where they live, and whether they are the ones this
//! code was written against.
//!
//! Weights are never bundled. They carry their own licences, they are large,
//! and a local-first tool should let the user decide what to download. Fetch
//! them with `cargo xtask fetch-model`, which verifies the archive and every
//! extracted file against the checksums pinned here.
//!
//! ## Why a model is described as a set of files
//!
//! DeepFilterNet3's ONNX export is not one model. It is three graphs — an
//! encoder and two decoders — and a `config.ini` stating the transform they
//! were trained through. There is no single-file export upstream, so a registry
//! entry describes a directory of files, each pinned separately. Hashing the
//! tarball would have been less code and worth less: it would not have caught a
//! single swapped decoder inside an otherwise-correct archive.

use std::path::{Path, PathBuf};

/// One file of a model.
#[derive(Clone, Copy, Debug)]
pub struct ModelFile {
    pub name: &'static str,
    /// SHA-256 of the file, verified before the model is trusted.
    pub sha256: &'static str,
}

/// Which role a file plays. The runner asks for graphs by role rather than by
/// file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Encoder,
    ErbDecoder,
    DfDecoder,
    /// Not read at run time — the values it states are compiled into
    /// [`crate::DFN3`] — but pinned so a mismatched export cannot be mistaken
    /// for the one those constants were derived from.
    Config,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Encoder, Self::ErbDecoder, Self::DfDecoder, Self::Config];
}

/// The archive the fetcher downloads, pinned by its own hash.
#[derive(Clone, Copy, Debug)]
pub struct Archive {
    pub url: &'static str,
    pub sha256: &'static str,
    /// Leading path components to strip when extracting.
    pub strip: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelSpec {
    pub id: &'static str,
    /// Directory holding this model's files, under [`model_directory`].
    pub directory: &'static str,
    /// The files that make up the model. Every one is verified; a model is the
    /// whole set or nothing.
    pub files: [(Role, ModelFile); 4],
    /// Sample rate the model is trained for. Nothing else may be fed to it.
    pub sample_rate: u32,
    /// Where a user can obtain it, and under what terms.
    pub source: &'static str,
    pub archive: Archive,
    pub licence: &'static str,
}

impl ModelSpec {
    pub fn file(&self, role: Role) -> Option<&ModelFile> {
        self.files
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, file)| file)
    }
}

/// Known models.
///
/// The hashes are of the files inside `models/DeepFilterNet3_onnx.tar.gz` in
/// the upstream repository, which is where the project's own author publishes
/// them — deliberately not one of the several community mirrors, which carry
/// the same bytes but not the same provenance.
pub const MODELS: [ModelSpec; 1] = [ModelSpec {
    id: "deepfilternet3",
    directory: "deepfilternet3",
    files: [
        (
            Role::Encoder,
            ModelFile {
                name: "enc.onnx",
                sha256: "7c5399d3da8a50ebef1c1a0ae421b33376aa5e45d0e92df16da7e83c9c131916",
            },
        ),
        (
            Role::ErbDecoder,
            ModelFile {
                name: "erb_dec.onnx",
                sha256: "ab669a1d10afe20911728b33053a452071042317a90581092b325da7b2f9d895",
            },
        ),
        (
            Role::DfDecoder,
            ModelFile {
                name: "df_dec.onnx",
                sha256: "23114ce3b0f6464b763ee62f7bb8aab6b2a129a21eabd5bcfe59413db05f278a",
            },
        ),
        (
            Role::Config,
            ModelFile {
                name: "config.ini",
                sha256: "415eb925d44990d938fb739f514aa3662c1ec0ea836cff044fa1291b82cb4290",
            },
        ),
    ],
    sample_rate: 48_000,
    source: "https://github.com/Rikorose/DeepFilterNet",
    archive: Archive {
        url: "https://github.com/Rikorose/DeepFilterNet/raw/main/models/DeepFilterNet3_onnx.tar.gz",
        sha256: "c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616",
        // The archive nests everything under tmp/export/.
        strip: 2,
    },
    licence: "MIT / Apache-2.0 (verify before redistributing)",
}];

/// The model trained for this rate, if there is one.
pub fn model_for(sample_rate: u32) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.sample_rate == sample_rate)
}

/// Where weights live. Overridable for tests and for a portable install.
pub fn model_directory() -> PathBuf {
    if let Ok(path) = std::env::var("AUDIO_LEVELLER_MODELS") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".audio-leveller").join("models")
}

/// Directory holding one model's files.
pub fn model_path(spec: &ModelSpec) -> PathBuf {
    model_directory().join(spec.directory)
}

/// Path of one file of a model.
pub fn model_file_path(spec: &ModelSpec, role: Role) -> Option<PathBuf> {
    Some(model_path(spec).join(spec.file(role)?.name))
}

/// SHA-256, hand-rolled.
///
/// A hash is a page of arithmetic with published test vectors, and this needs
/// exactly one of them. Reaching for a crate here would add a dependency to
/// the tree of a program whose whole reason for using it is to be careful
/// about what it trusts.
pub fn sha256(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1, 0x923f_82a4,
        0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3, 0x72be_5d74, 0x80de_b1fe,
        0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f,
        0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da, 0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
        0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc,
        0x5338_0d13, 0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
        0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070, 0x19a4_c116,
        0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
        0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208, 0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7,
        0xc671_78f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let mut message = bytes.to_vec();
    let bit_length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    let mut w = [0u32; 64];
    for block in message.as_chunks::<64>().0 {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let temp1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let temp2 = s0.wrapping_add(maj);

            v = [
                temp1.wrapping_add(temp2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(temp1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (h, v) in h.iter_mut().zip(v) {
            *h = h.wrapping_add(v);
        }
    }

    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// Verify one file against its expected hash.
///
/// An empty expected hash is a failure, not a pass. Weights are executable
/// intent: a registry entry nobody has pinned yet must not act as a wildcard
/// that accepts whatever happens to be sitting at that path.
pub fn verify_checksum(file: &ModelFile, path: &Path) -> Result<(), String> {
    if file.sha256.is_empty() {
        return Err(format!(
            "no checksum pinned for {}; refusing to load unverified weights",
            file.name
        ));
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Err(format!("not found at {}", path.display()));
    };
    let digest = sha256(&bytes);
    if digest != file.sha256 {
        return Err(format!(
            "checksum mismatch for {} (got {}…)",
            file.name,
            &digest[..16]
        ));
    }
    Ok(())
}

/// Verify every file of a model.
///
/// A model is all of its files or none of them — an encoder that matches paired
/// with a decoder that does not is not a partially usable model, it is an
/// unknown one.
pub fn verify_model(spec: &ModelSpec) -> Result<(), String> {
    for (role, file) in &spec.files {
        let Some(path) = model_file_path(spec, *role) else {
            return Err(format!("{}: no path for {:?}", spec.id, role));
        };
        verify_checksum(file, &path).map_err(|reason| format!("{}: {reason}", spec.id))?;
    }
    Ok(())
}

/// How much disk a model's files take, for the report and for a UI that wants
/// to say what a download would cost.
pub fn installed_bytes(spec: &ModelSpec) -> u64 {
    spec.files
        .iter()
        .filter_map(|(role, _)| model_file_path(spec, *role))
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vectors() {
        // The whole trust story rests on this function, so it is checked
        // against the values in the standard rather than against itself.
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_handles_a_block_boundary() {
        // The padding is where a hand-rolled hash goes wrong, and it goes wrong
        // exactly at the lengths where a length field no longer fits in the
        // final block.
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        for length in [55usize, 56, 57, 63, 64, 65] {
            // Not a known vector, just that it produces something of the right
            // shape rather than panicking on the boundary.
            assert_eq!(sha256(&vec![0u8; length]).len(), 64);
        }
    }

    #[test]
    fn an_unpinned_file_is_refused_rather_than_accepted() {
        // A registry entry nobody has pinned must not act as a wildcard.
        let file = ModelFile {
            name: "enc.onnx",
            sha256: "",
        };
        let error = verify_checksum(&file, Path::new("/nonexistent")).unwrap_err();
        assert!(error.contains("refusing to load unverified weights"), "{error}");
    }

    #[test]
    fn a_missing_file_says_where_it_looked() {
        let file = ModelFile {
            name: "enc.onnx",
            sha256: "00",
        };
        let error = verify_checksum(&file, Path::new("/nonexistent/enc.onnx")).unwrap_err();
        assert!(error.contains("/nonexistent/enc.onnx"), "{error}");
    }

    #[test]
    fn a_file_whose_bytes_have_drifted_is_refused() {
        let dir = std::env::temp_dir().join(format!("leveller-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("enc.onnx");
        std::fs::write(&path, b"abc").unwrap();

        let right = ModelFile {
            name: "enc.onnx",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verify_checksum(&right, &path).is_ok());

        let wrong = ModelFile {
            name: "enc.onnx",
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        };
        let error = verify_checksum(&wrong, &path).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_registered_model_pins_every_file_it_names() {
        for spec in &MODELS {
            for role in Role::ALL {
                let file = spec
                    .file(role)
                    .unwrap_or_else(|| panic!("{} has no {role:?}", spec.id));
                assert_eq!(
                    file.sha256.len(),
                    64,
                    "{} / {role:?} is not pinned to a full SHA-256",
                    spec.id
                );
            }
            assert_eq!(spec.archive.sha256.len(), 64, "{} archive", spec.id);
        }
    }

    #[test]
    fn the_model_directory_can_be_pointed_somewhere_else() {
        // The tests and a portable install both need this, and a hard-coded
        // home directory would make the whole registry untestable.
        // SAFETY: single-threaded test, and the variable is read on the next
        // line rather than stored.
        unsafe { std::env::set_var("AUDIO_LEVELLER_MODELS", "/tmp/somewhere") };
        assert_eq!(model_directory(), Path::new("/tmp/somewhere"));
        unsafe { std::env::remove_var("AUDIO_LEVELLER_MODELS") };
    }

    #[test]
    fn there_is_a_model_for_forty_eight_kilohertz_and_nothing_else() {
        // The model is trained at one rate, and resampling into and out of it
        // inside the stage would spend two conversions to reach a denoiser that
        // is not obviously better than the classical one needing neither.
        assert!(model_for(48_000).is_some());
        assert!(model_for(44_100).is_none());
    }
}
