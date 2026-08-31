//! Native, deterministic token-compression primitives.
//!
//! This crate deliberately has no network, process, or persistence dependency.
//! Endpoint adapters can apply [`InputTransform`] to request parts in memory.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const INPUT_COMPRESSION_ID: &str = "native-rle";
pub const INPUT_COMPRESSION_VERSION: &str = "1";
pub const REGISTER_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegisterId {
    Compact,
}

impl RegisterId {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Compact => "compact",
        }
    }

    /// The fidelity clauses are intentionally direct and machine-testable.
    pub const fn contract(self) -> &'static str {
        match self {
            Self::Compact => {
                r#"## Output register: compact@1

Compress only surrounding natural-language prose. Preserve every code span,
fenced code block, diff, file path, identifier, scope declaration, and
structured machine-parsed output byte-for-byte. Do not abbreviate, reorder,
reformat, or otherwise alter protected content. Review findings JSON must
remain valid against its original schema. End terminal prose with
`output-register: compact@1`."#
            }
        }
    }
}

impl std::str::FromStr for RegisterId {
    type Err = CompressionError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "compact" => Ok(Self::Compact),
            _ => Err(CompressionError::UnknownRegister(value.into())),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompressionError {
    #[error("unknown output register '{0}'")]
    UnknownRegister(String),
    #[error("invalid familiar compressed input")]
    InvalidInput,
}

/// An ordered provider payload part. Cache-control parts are opaque and are
/// never transformed, copied into a side channel, logged, or persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderPart<'a> {
    Content(&'a [u8]),
    CacheControl(&'a [u8]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformedPart {
    Content(Vec<u8>),
    CacheControl(Vec<u8>),
}

/// In-memory request envelope for same-machine endpoint adapters. Header
/// values (including authorization) are opaque and forwarded unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRequest {
    pub headers: Vec<(String, Vec<u8>)>,
    pub parts: Vec<EndpointPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointPart {
    Content(Vec<u8>),
    CacheControl(Vec<u8>),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EndpointMode;

impl EndpointMode {
    /// Transform and immediately forward one request. No request or response
    /// is retained by this API, and credential headers are never interpreted.
    pub fn forward<R>(
        &self,
        request: EndpointRequest,
        forwarder: impl FnOnce(EndpointRequest) -> R,
    ) -> R {
        let parts = request
            .parts
            .into_iter()
            .map(|part| match part {
                EndpointPart::Content(bytes) => {
                    EndpointPart::Content(InputTransform.compress(&bytes))
                }
                EndpointPart::CacheControl(bytes) => EndpointPart::CacheControl(bytes),
            })
            .collect();
        forwarder(EndpointRequest {
            headers: request.headers,
            parts,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InputTransform;

impl InputTransform {
    pub const fn identity(&self) -> (&'static str, &'static str) {
        (INPUT_COMPRESSION_ID, INPUT_COMPRESSION_VERSION)
    }

    /// PackBits-style encoding. Runs are emitted only when shorter. The fixed
    /// framing and absence of state, clocks, randomness, or platform data make
    /// this deterministic across calls and process restarts.
    pub fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut out = b"FAIC\x01".to_vec();
        let mut index = 0;
        while index < input.len() {
            let mut run = 1usize;
            while index + run < input.len() && input[index + run] == input[index] && run < 128 {
                run += 1;
            }
            if run >= 3 {
                out.push(0x80 | (run as u8 - 1));
                out.push(input[index]);
                index += run;
                continue;
            }
            let start = index;
            index += run;
            while index < input.len() && index - start < 128 {
                let mut next_run = 1usize;
                while index + next_run < input.len()
                    && input[index + next_run] == input[index]
                    && next_run < 3
                {
                    next_run += 1;
                }
                if next_run >= 3 {
                    break;
                }
                if index + next_run - start > 128 {
                    break;
                }
                index += next_run;
            }
            let length = index - start;
            out.push(length as u8 - 1);
            out.extend_from_slice(&input[start..index]);
        }
        out
    }

    pub fn decompress(&self, input: &[u8]) -> Result<Vec<u8>, CompressionError> {
        let Some(mut rest) = input.strip_prefix(b"FAIC\x01") else {
            return Err(CompressionError::InvalidInput);
        };
        let mut out = Vec::new();
        while let Some((&tag, tail)) = rest.split_first() {
            rest = tail;
            let length = usize::from(tag & 0x7f) + 1;
            if tag & 0x80 != 0 {
                let Some((&byte, tail)) = rest.split_first() else {
                    return Err(CompressionError::InvalidInput);
                };
                out.resize(out.len() + length, byte);
                rest = tail;
            } else {
                if rest.len() < length {
                    return Err(CompressionError::InvalidInput);
                }
                let (literal, tail) = rest.split_at(length);
                out.extend_from_slice(literal);
                rest = tail;
            }
        }
        Ok(out)
    }

    pub fn transform_parts(&self, parts: &[ProviderPart<'_>]) -> Vec<TransformedPart> {
        parts
            .iter()
            .map(|part| match part {
                ProviderPart::Content(bytes) => TransformedPart::Content(self.compress(bytes)),
                ProviderPart::CacheControl(bytes) => TransformedPart::CacheControl(bytes.to_vec()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_bytes_round_trip() {
        let input = (0..=255).cycle().take(4097).collect::<Vec<_>>();
        let encoded = InputTransform.compress(&input);
        assert_eq!(InputTransform.decompress(&encoded).unwrap(), input);
    }

    #[test]
    fn deterministic_and_prefix_chunks_are_stable() {
        let prefix = b"same stable prefix";
        assert_eq!(
            InputTransform.compress(prefix),
            InputTransform.compress(prefix)
        );
        let first = InputTransform.transform_parts(&[ProviderPart::Content(prefix)]);
        let longer = InputTransform.transform_parts(&[
            ProviderPart::Content(prefix),
            ProviderPart::Content(b"volatile suffix"),
        ]);
        assert_eq!(first[0], longer[0]);
    }

    #[test]
    fn cache_controls_are_verbatim_and_unmoved() {
        let marker = br#"{"cache_control":{"type":"ephemeral"}}"#;
        let transformed = InputTransform.transform_parts(&[
            ProviderPart::Content(b"prefix"),
            ProviderPart::CacheControl(marker),
            ProviderPart::Content(b"suffix"),
        ]);
        assert_eq!(
            transformed[1],
            TransformedPart::CacheControl(marker.to_vec())
        );
    }

    #[test]
    fn endpoint_forwards_credentials_opaque_and_retains_nothing() {
        let request = EndpointRequest {
            headers: vec![("authorization".into(), b"Bearer fixture".to_vec())],
            parts: vec![EndpointPart::CacheControl(b"break".to_vec())],
        };
        let forwarded = EndpointMode.forward(request.clone(), |value| value);
        assert_eq!(forwarded.headers, request.headers);
        assert_eq!(forwarded.parts, request.parts);
    }
}
