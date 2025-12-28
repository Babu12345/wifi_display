//! Crate for decoding MQTT display data
//! Provides base64 decoding for e-ink display frame chunks
#![no_std]
#![deny(missing_docs)]

use base64ct::{Base64, Encoding};
use serde::Deserialize;

/// JSON payload structure for raw binary display data
/// Expected format: {"frame": "base64_encoded_string", "requires_response": true/false, "chunk_index": 0, "total_chunks": 1}
/// The frame data is base64-encoded raw frame data chunk
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct BinaryPayload<'a> {
    #[serde(borrow)]
    /// Base64-encoded frame data chunk
    pub frame: &'a str,
    /// Whether the client requires a response
    pub requires_response: bool,
    /// Index of this chunk (0-based)
    pub chunk_index: usize,
    /// Total number of chunks
    pub total_chunks: usize,
}

/// Metadata about a chunk extracted from JSON payload
#[derive(Debug, Clone, Copy)]
pub struct ChunkMetadata {
    /// Whether the client requires a response
    pub requires_response: bool,
    /// Index of this chunk (0-based)
    pub chunk_index: usize,
    /// Total number of chunks
    pub total_chunks: usize,
}

/// Decode a JSON chunk containing base64-encoded frame data
///
/// # Arguments
/// * `json_data` - JSON payload bytes
/// * `output` - Output buffer for decoded frame data
///
/// # Returns
/// Tuple of (decoded_length, chunk_metadata) or error
pub fn decode_chunk(
    json_data: &[u8],
    output: &mut [u8],
) -> Result<(usize, ChunkMetadata), &'static str> {
    // Parse JSON
    let json_end = json_data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(json_data.len());
    let parsed: BinaryPayload = serde_json_core::from_slice(&json_data[..json_end])
        .map_err(|_| "Failed to parse JSON")?
        .0;

    let metadata = ChunkMetadata {
        requires_response: parsed.requires_response,
        chunk_index: parsed.chunk_index,
        total_chunks: parsed.total_chunks,
    };

    // Decode base64 directly to output
    let decoded_len = Base64::decode(parsed.frame, output)
        .map_err(|_| "Failed to decode base64")?
        .len();

    Ok((decoded_len, metadata))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    /// Encode data to base64 string (test helper)
    fn base64_encode<'a>(data: &[u8], output: &'a mut [u8]) -> Result<&'a str, &'static str> {
        Base64::encode(data, output).map_err(|_| "Failed to encode base64 data")
    }

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let mut encoded = [0u8; 100];
        let mut decoded = [0u8; 100];

        let encoded_str = base64_encode(original, &mut encoded).unwrap();
        let decoded_slice = Base64::decode(encoded_str, &mut decoded).unwrap();

        assert_eq!(decoded_slice, original);
    }

    #[test]
    fn test_decode_chunk_simple() {
        // Raw frame data (not RLE compressed)
        let original = [0xFF, 0xFF, 0xFF, 0x00, 0x00];

        let mut base64_buf = [0u8; 50];
        let b64_str = base64_encode(&original, &mut base64_buf).unwrap();

        // Create JSON payload with chunk metadata
        let json_string = std::format!(
            r#"{{"frame":"{}","requires_response":true,"chunk_index":0,"total_chunks":1}}"#,
            b64_str
        );
        let mut output = [0u8; 100];

        let (len, metadata) = decode_chunk(json_string.as_bytes(), &mut output).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&output[..len], &original);
        assert_eq!(metadata.requires_response, true);
        assert_eq!(metadata.chunk_index, 0);
        assert_eq!(metadata.total_chunks, 1);
    }

    #[test]
    fn test_decode_chunk_large() {
        // Simulate e-ink display data
        let mut original = [0xFF; 1000];
        for i in 100..110 {
            original[i] = 0x00;
        }

        let mut base64_buf = [0u8; 2000];
        let b64_str = base64_encode(&original, &mut base64_buf).unwrap();

        let json_string = std::format!(
            r#"{{"frame":"{}","requires_response":false,"chunk_index":0,"total_chunks":1}}"#,
            b64_str
        );
        let mut output = vec![0u8; 2000];

        let (len, metadata) = decode_chunk(json_string.as_bytes(), &mut output).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&output[..len], &original[..]);
        assert_eq!(metadata.requires_response, false);
    }

    #[test]
    fn test_decode_chunk_invalid_json() {
        let json = b"{invalid json}";
        let mut output = [0u8; 100];
        let result = decode_chunk(json, &mut output);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_chunk_invalid_base64() {
        let json = b"{\"frame\":\"!!!invalid_base64!!!\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let mut output = [0u8; 500];
        let result = decode_chunk(json, &mut output);
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_payload_parsing() {
        let json =
            b"{\"frame\":\"AQID\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let parsed: BinaryPayload = serde_json_core::from_slice(json).unwrap().0;
        assert_eq!(parsed.frame, "AQID");
        assert_eq!(parsed.requires_response, true);
        assert_eq!(parsed.chunk_index, 0);
        assert_eq!(parsed.total_chunks, 1);
    }

    #[test]
    fn test_multi_chunk_reassembly() {
        // Test encoding and decoding data split into multiple chunks
        const CHUNK_SIZE: usize = 500;
        let original = [0xFF; 1500];

        let num_chunks = (original.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;

        let mut reassembled = vec![0u8; original.len()];
        let mut offset = 0;

        for chunk_idx in 0..num_chunks {
            let chunk_start = chunk_idx * CHUNK_SIZE;
            let chunk_end = core::cmp::min(chunk_start + CHUNK_SIZE, original.len());
            let chunk_data = &original[chunk_start..chunk_end];

            // Base64 encode this chunk directly (no RLE)
            let mut base64_buf = vec![0u8; chunk_data.len() * 2];
            let b64_str = base64_encode(chunk_data, &mut base64_buf).unwrap();

            let is_last_chunk = chunk_idx == num_chunks - 1;
            let json_string = std::format!(
                r#"{{"frame":"{}","requires_response":{},"chunk_index":{},"total_chunks":{}}}"#,
                b64_str,
                is_last_chunk,
                chunk_idx,
                num_chunks
            );

            let mut output = vec![0u8; 2000];
            let (decoded_len, metadata) =
                decode_chunk(json_string.as_bytes(), &mut output).unwrap();

            assert_eq!(metadata.chunk_index, chunk_idx);
            assert_eq!(metadata.total_chunks, num_chunks);
            assert_eq!(metadata.requires_response, is_last_chunk);
            assert_eq!(decoded_len, chunk_data.len());

            reassembled[offset..offset + decoded_len].copy_from_slice(&output[..decoded_len]);
            offset += decoded_len;
        }

        assert_eq!(offset, original.len());
        assert_eq!(&reassembled[..offset], &original[..]);
    }
}
