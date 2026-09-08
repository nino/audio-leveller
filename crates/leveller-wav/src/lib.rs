//! RIFF/WAVE reading and writing, with no dependencies worth the name.
//!
//! The formats that actually turn up in spoken-word audio:
//!
//! - 8, 16, 24 and 32-bit signed integer PCM (`WAVE_FORMAT_PCM`)
//! - 32 and 64-bit IEEE float (`WAVE_FORMAT_IEEE_FLOAT`)
//! - any of the above inside `WAVE_FORMAT_EXTENSIBLE`
//!
//! Samples decode to `f32` in [−1, 1). The file's own bit depth and sample
//! format travel with the audio so it can be written back the way it arrived —
//! a 24-bit recording should not quietly become 16-bit because it passed
//! through here.
//!
//! Hand-rolled rather than delegated to a crate for one reason: decode followed
//! by encode has to return the bytes that came in, and that is a promise about
//! quantisation and header layout that a general-purpose library does not make.

use leveller_dsp::Signal;

/// Whether the samples on disk are integers or floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    Int,
    Float,
}

/// Audio plus the encoding details of the file it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub signal: Signal,
    /// Bit depth of the source file, used when re-encoding.
    pub bit_depth: u16,
    pub format: SampleFormat,
}

impl Audio {
    /// Re-attach an existing file's encoding to a processed signal.
    pub fn like(signal: Signal, source: &Audio) -> Self {
        Self {
            signal,
            bit_depth: source.bit_depth,
            format: source.format,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WavError {
    #[error("not a RIFF file")]
    NotRiff,
    #[error("not a WAVE file")]
    NotWave,
    #[error("the file ends inside a chunk header")]
    Truncated,
    #[error("the file has no fmt chunk")]
    NoFormat,
    #[error("the file has no data chunk")]
    NoData,
    #[error("the file declares no channels")]
    NoChannels,
    #[error("unsupported {format} bit depth: {bits}")]
    UnsupportedDepth { format: &'static str, bits: u16 },
}

const FORMAT_PCM: u16 = 0x0001;
const FORMAT_IEEE_FLOAT: u16 = 0x0003;
const FORMAT_EXTENSIBLE: u16 = 0xfffe;

/// Size of the canonical 44-byte header this encoder writes.
const HEADER_LEN: usize = 44;

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Decode a WAV file.
pub fn decode(bytes: &[u8]) -> Result<Audio, WavError> {
    if bytes.len() < 12 {
        return Err(WavError::Truncated);
    }
    if &bytes[0..4] != b"RIFF" {
        return Err(WavError::NotRiff);
    }
    if &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }

    let mut audio_format = FORMAT_PCM;
    let mut channel_count = 0usize;
    let mut sample_rate = 0u32;
    let mut bits = 0u16;
    let mut data: Option<&[u8]> = None;
    let mut saw_format = false;

    // Walk the chunk list. Chunks are word-aligned: an odd-sized one is
    // followed by a pad byte that is not counted in its size.
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4) as usize;
        let body = pos + 8;
        // A truncated final chunk is common enough in the wild — a recorder cut
        // off mid-write — that it is worth taking what is there rather than
        // refusing the file.
        let end = body.saturating_add(size).min(bytes.len());

        if id == b"fmt " && end - body >= 16 {
            saw_format = true;
            audio_format = u16_at(bytes, body);
            channel_count = usize::from(u16_at(bytes, body + 2));
            sample_rate = u32_at(bytes, body + 4);
            bits = u16_at(bytes, body + 14);
            // In an extensible chunk the real format is the first two bytes of
            // the SubFormat GUID.
            if audio_format == FORMAT_EXTENSIBLE && end - body >= 26 {
                audio_format = u16_at(bytes, body + 24);
            }
        } else if id == b"data" {
            data = Some(&bytes[body..end]);
        }

        pos = body + size + (size & 1);
    }

    if !saw_format {
        return Err(WavError::NoFormat);
    }
    let data = data.ok_or(WavError::NoData)?;
    if channel_count == 0 {
        return Err(WavError::NoChannels);
    }

    let format = if audio_format == FORMAT_IEEE_FLOAT {
        SampleFormat::Float
    } else {
        SampleFormat::Int
    };
    let bytes_per_sample = usize::from(bits) / 8;
    if bytes_per_sample == 0 {
        return Err(WavError::UnsupportedDepth {
            format: format_name(format),
            bits,
        });
    }
    let read = sample_reader(format, bits)?;

    let frame_size = bytes_per_sample * channel_count;
    let frames = data.len() / frame_size;

    let mut channels = vec![vec![0.0f32; frames]; channel_count];
    for i in 0..frames {
        let frame = i * frame_size;
        for (c, channel) in channels.iter_mut().enumerate() {
            channel[i] = read(&data[frame + c * bytes_per_sample..]);
        }
    }

    Ok(Audio {
        signal: Signal::new(sample_rate, channels),
        bit_depth: bits,
        format,
    })
}

/// Encode to a canonical 44-byte-header WAV, in the audio's own format.
pub fn encode(audio: &Audio) -> Result<Vec<u8>, WavError> {
    encode_as(audio, audio.bit_depth, audio.format)
}

/// Encode at a chosen bit depth and format, whatever the audio arrived as.
pub fn encode_as(audio: &Audio, bit_depth: u16, format: SampleFormat) -> Result<Vec<u8>, WavError> {
    let write = sample_writer(format, bit_depth)?;

    let signal = &audio.signal;
    let channel_count = signal.channel_count();
    let bytes_per_sample = usize::from(bit_depth) / 8;
    let frame_size = bytes_per_sample * channel_count;
    let data_len = signal.len() * frame_size;

    let mut out = Vec::with_capacity(HEADER_LEN + data_len);
    let audio_format = match format {
        SampleFormat::Float => FORMAT_IEEE_FLOAT,
        SampleFormat::Int => FORMAT_PCM,
    };

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&audio_format.to_le_bytes());
    out.extend_from_slice(&(channel_count as u16).to_le_bytes());
    out.extend_from_slice(&signal.sample_rate().to_le_bytes());
    out.extend_from_slice(&(signal.sample_rate() * frame_size as u32).to_le_bytes());
    out.extend_from_slice(&(frame_size as u16).to_le_bytes());
    out.extend_from_slice(&bit_depth.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());

    out.resize(HEADER_LEN + data_len, 0);
    for i in 0..signal.len() {
        let frame = HEADER_LEN + i * frame_size;
        for c in 0..channel_count {
            let at = frame + c * bytes_per_sample;
            write(&mut out[at..at + bytes_per_sample], signal.channel(c)[i]);
        }
    }

    Ok(out)
}

fn format_name(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::Float => "float",
        SampleFormat::Int => "PCM",
    }
}

type Reader = fn(&[u8]) -> f32;

fn sample_reader(format: SampleFormat, bits: u16) -> Result<Reader, WavError> {
    let reader: Reader = match (format, bits) {
        (SampleFormat::Float, 32) => |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        (SampleFormat::Float, 64) => |b| {
            f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
        },
        (SampleFormat::Int, 8) => |b| (f32::from(b[0]) - 128.0) / 128.0,
        (SampleFormat::Int, 16) => |b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
        (SampleFormat::Int, 24) => |b| {
            // Sign-extend the top byte into a full i32 by shifting up and back.
            let raw = i32::from_le_bytes([0, b[0], b[1], b[2]]);
            (raw >> 8) as f32 / 8_388_608.0
        },
        (SampleFormat::Int, 32) => {
            |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
        }
        _ => {
            return Err(WavError::UnsupportedDepth {
                format: format_name(format),
                bits,
            });
        }
    };
    Ok(reader)
}

/// Round half away from zero, then clamp to the format's range.
///
/// Signed PCM is asymmetric — −32768 to 32767 — and the tempting fix, scaling
/// the write side by 32767 to match, would make every decoded sample come back
/// one LSB short and a decode/encode round trip lossy. Scaling by the same
/// power of two the reader divides by and clamping only true full scale is the
/// version that round-trips; the one clamped sample is inherent to the format.
fn quantize(value: f32, scale: f64, min: i64, max: i64) -> i64 {
    let scaled = (f64::from(value) * scale).round() as i64;
    scaled.clamp(min, max)
}

type Writer = fn(&mut [u8], f32);

fn sample_writer(format: SampleFormat, bits: u16) -> Result<Writer, WavError> {
    let writer: Writer = match (format, bits) {
        (SampleFormat::Float, 32) => |b, v| b.copy_from_slice(&v.to_le_bytes()),
        (SampleFormat::Float, 64) => |b, v| b.copy_from_slice(&f64::from(v).to_le_bytes()),
        (SampleFormat::Int, 8) => |b, v| b[0] = (quantize(v, 128.0, -128, 127) + 128) as u8,
        (SampleFormat::Int, 16) => |b, v| {
            b.copy_from_slice(&(quantize(v, 32768.0, -32768, 32767) as i16).to_le_bytes());
        },
        (SampleFormat::Int, 24) => |b, v| {
            let s = quantize(v, 8_388_608.0, -8_388_608, 8_388_607) as i32;
            b.copy_from_slice(&s.to_le_bytes()[0..3]);
        },
        (SampleFormat::Int, 32) => |b, v| {
            let s = quantize(v, 2_147_483_648.0, -2_147_483_648, 2_147_483_647) as i32;
            b.copy_from_slice(&s.to_le_bytes());
        },
        _ => {
            return Err(WavError::UnsupportedDepth {
                format: format_name(format),
                bits,
            });
        }
    };
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn tone(frames: usize, channels: usize) -> Signal {
        Signal::new(
            48_000,
            (0..channels)
                .map(|c| {
                    (0..frames)
                        .map(|i| {
                            (0.5 * (TAU * (i as f64) / 32.0 + c as f64).sin()) as f32
                        })
                        .collect()
                })
                .collect(),
        )
    }

    fn audio(bit_depth: u16, format: SampleFormat) -> Audio {
        Audio {
            signal: tone(512, 2),
            bit_depth,
            format,
        }
    }

    #[test]
    fn a_round_trip_returns_the_same_bytes() {
        for (bits, format) in [
            (8, SampleFormat::Int),
            (16, SampleFormat::Int),
            (24, SampleFormat::Int),
            (32, SampleFormat::Int),
            (32, SampleFormat::Float),
            (64, SampleFormat::Float),
        ] {
            let original = encode(&audio(bits, format)).unwrap();
            let decoded = decode(&original).unwrap();
            assert_eq!(decoded.bit_depth, bits);
            assert_eq!(decoded.format, format);
            let again = encode(&decoded).unwrap();
            assert_eq!(again, original, "{bits}-bit {format:?} did not round trip");
        }
    }

    #[test]
    fn float_samples_survive_exactly() {
        let source = audio(32, SampleFormat::Float);
        let decoded = decode(&encode(&source).unwrap()).unwrap();
        assert_eq!(decoded.signal, source.signal);
    }

    #[test]
    fn sixteen_bit_quantisation_costs_less_than_one_step() {
        let source = audio(16, SampleFormat::Int);
        let decoded = decode(&encode(&source).unwrap()).unwrap();
        for (a, b) in decoded
            .signal
            .channel(0)
            .iter()
            .zip(source.signal.channel(0))
        {
            assert!((a - b).abs() <= 1.0 / 32768.0, "{a} vs {b}");
        }
    }

    #[test]
    fn full_scale_is_the_only_sample_that_clamps() {
        // −1.0 is representable in signed PCM and +1.0 is not, which is the
        // format's asymmetry and not something the codec should paper over.
        let signal = Signal::mono(48_000, vec![-1.0, 1.0, 0.0, 0.5]);
        let source = Audio {
            signal,
            bit_depth: 16,
            format: SampleFormat::Int,
        };
        let decoded = decode(&encode(&source).unwrap()).unwrap();
        let got = decoded.signal.channel(0);
        assert_eq!(got[0], -1.0);
        assert!((got[1] - 32767.0 / 32768.0).abs() < 1e-9, "{}", got[1]);
        assert_eq!(got[2], 0.0);
        assert_eq!(got[3], 0.5);
    }

    #[test]
    fn channels_stay_in_their_lanes() {
        let signal = Signal::new(44_100, vec![vec![0.25, 0.25], vec![-0.5, -0.5]]);
        let source = Audio {
            signal: signal.clone(),
            bit_depth: 24,
            format: SampleFormat::Int,
        };
        let decoded = decode(&encode(&source).unwrap()).unwrap();
        assert_eq!(decoded.signal.sample_rate(), 44_100);
        assert_eq!(decoded.signal.channels(), signal.channels());
    }

    #[test]
    fn the_header_says_what_the_data_is() {
        let bytes = encode(&audio(24, SampleFormat::Int)).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
        assert_eq!(u16_at(&bytes, 22), 2, "channels");
        assert_eq!(u32_at(&bytes, 24), 48_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 48_000 * 6, "byte rate");
        assert_eq!(u16_at(&bytes, 32), 6, "block align");
        assert_eq!(u16_at(&bytes, 34), 24, "bits");
        assert_eq!(u32_at(&bytes, 40) as usize, bytes.len() - HEADER_LEN);
    }

    #[test]
    fn an_extensible_header_is_read_through_to_its_real_format() {
        // 40-byte fmt chunk: format 0xFFFE, with FORMAT_IEEE_FLOAT at the head
        // of the SubFormat GUID.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // patched below
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&FORMAT_EXTENSIBLE.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // channels
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes()); // bits
        bytes.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        bytes.extend_from_slice(&32u16.to_le_bytes()); // valid bits
        bytes.extend_from_slice(&0u32.to_le_bytes()); // channel mask
        bytes.extend_from_slice(&FORMAT_IEEE_FLOAT.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 14]); // rest of the GUID
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&0.25f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.75f32).to_le_bytes());
        let len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());

        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.format, SampleFormat::Float);
        assert_eq!(decoded.bit_depth, 32);
        assert_eq!(decoded.signal.channel(0), &[0.25, -0.75]);
    }

    #[test]
    fn an_odd_sized_chunk_is_stepped_over_with_its_pad_byte() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        // A 3-byte LIST chunk, so the pad byte matters: miss it and the walk
        // lands one byte off and finds no data chunk at all.
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc\0");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&FORMAT_PCM.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&16_384i16.to_le_bytes());
        bytes.extend_from_slice(&(-16_384i16).to_le_bytes());
        let len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());

        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.signal.channel(0), &[0.5, -0.5]);
    }

    #[test]
    fn nonsense_is_refused_with_a_reason() {
        assert_eq!(decode(b"nope").unwrap_err(), WavError::Truncated);
        assert_eq!(
            decode(b"RIFX\0\0\0\0WAVE").unwrap_err(),
            WavError::NotRiff
        );
        assert_eq!(
            decode(b"RIFF\0\0\0\0AIFF").unwrap_err(),
            WavError::NotWave
        );
        assert_eq!(
            decode(b"RIFF\0\0\0\0WAVE").unwrap_err(),
            WavError::NoFormat
        );
        assert_eq!(
            encode_as(&audio(16, SampleFormat::Int), 12, SampleFormat::Int).unwrap_err(),
            WavError::UnsupportedDepth {
                format: "PCM",
                bits: 12
            }
        );
    }

    #[test]
    fn a_truncated_data_chunk_yields_the_frames_that_are_there() {
        let mut bytes = encode(&audio(16, SampleFormat::Int)).unwrap();
        let full = decode(&bytes).unwrap().signal.len();
        bytes.truncate(bytes.len() - 40);
        let short = decode(&bytes).unwrap().signal.len();
        assert_eq!(short, full - 10, "40 bytes is 10 stereo 16-bit frames");
    }
}
