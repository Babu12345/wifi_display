//! Crate for encoding and decoding MQTT display data
//! Provides RLE compression and base64 encoding/decoding for e-ink display frames
#![no_std]
#![deny(missing_docs)]

use base64ct::{Base64, Encoding};
use serde::Deserialize;

/// JSON payload structure for raw binary display data
/// Expected format: {"frame": "base64_encoded_string", "requires_response": true/false}
/// The frame data is base64-encoded RLE compressed data
/// RLE format: [count, byte, count, byte, ...] where count is u8 (1-255)
#[derive(Deserialize)]
pub struct BinaryPayload<'a> {
    #[serde(borrow)]
    /// Base64-encoded RLE compressed frame data
    pub frame: &'a str,
    /// Whether the client requires a response
    pub requires_response: bool,
}

/// RLE encoder for binary data
/// Format: [count, byte, count, byte, ...]
///
/// # Arguments
/// * `data` - Input data to compress
/// * `output` - Output buffer for compressed data
///
/// # Returns
/// Number of bytes written to output
///
/// # Example
/// ```
/// let input = [0xFF, 0xFF, 0xFF, 0x00, 0x00];
/// let mut output = [0u8; 10];
/// let len = decoding::rle_encode(&input, &mut output);
/// assert_eq!(&output[..len], &[3, 0xFF, 2, 0x00]);
/// ```
pub fn rle_encode(data: &[u8], output: &mut [u8]) -> usize {
    let mut out_pos = 0;
    let mut i = 0;

    while i < data.len() {
        let byte = data[i];
        let mut count = 1u8;

        // Count consecutive identical bytes (max 255)
        while i + (count as usize) < data.len()
              && data[i + (count as usize)] == byte
              && count < 255
        {
            count += 1;
        }

        output[out_pos] = count;
        output[out_pos + 1] = byte;
        out_pos += 2;
        i += count as usize;
    }

    out_pos
}

/// RLE decoder that works in-place
/// Format: [count, byte, count, byte, ...]
///
/// # Arguments
/// * `buffer` - Buffer containing compressed data at the beginning; will be overwritten with decompressed data
/// * `compressed_len` - Length of the compressed data
///
/// # Returns
/// Number of decompressed bytes
///
/// # Strategy
/// Moves compressed data to the end of the buffer to avoid overlap during decompression,
/// then reads from end and writes to beginning
///
/// # Example
/// ```
/// let mut buffer = [0u8; 100];
/// buffer[0] = 3;  // count
/// buffer[1] = 0xFF; // byte
/// buffer[2] = 2;  // count
/// buffer[3] = 0x00; // byte
///
/// let len = decoding::rle_decode_inplace(&mut buffer, 4);
/// assert_eq!(len, 5);
/// assert_eq!(&buffer[..5], &[0xFF, 0xFF, 0xFF, 0x00, 0x00]);
/// ```
pub fn rle_decode_inplace(buffer: &mut [u8], compressed_len: usize) -> usize {
    // Move compressed data to the end to avoid overlap during decompression
    let src_offset = buffer.len() - compressed_len;
    buffer.copy_within(0..compressed_len, src_offset);

    let mut read_pos = src_offset;
    let mut write_pos = 0;

    while read_pos < buffer.len() {
        let count = buffer[read_pos] as usize;
        let byte = buffer[read_pos + 1];

        // Write decompressed bytes
        for _ in 0..count {
            buffer[write_pos] = byte;
            write_pos += 1;
        }

        read_pos += 2;
    }

    write_pos
}

/// Decode base64 string in-place into a buffer
///
/// # Arguments
/// * `base64_str` - Base64 encoded string
/// * `output` - Output buffer for decoded data
///
/// # Returns
/// Number of decoded bytes or error message
pub fn base64_decode_inplace<'a>(
    base64_str: &str,
    output: &'a mut [u8],
) -> Result<&'a [u8], &'static str> {
    Base64::decode(base64_str, output)
        .map_err(|_| "Failed to decode base64 data")
}

/// Encode data to base64 string
///
/// # Arguments
/// * `data` - Input data to encode
/// * `output` - Output buffer for base64 string
///
/// # Returns
/// Base64 encoded string or error
pub fn base64_encode<'a>(
    data: &[u8],
    output: &'a mut [u8],
) -> Result<&'a str, &'static str> {
    Base64::encode(data, output)
        .map_err(|_| "Failed to encode base64 data")
}

/// Decode JSON payload containing base64-encoded RLE data
///
/// This function performs the complete decoding pipeline:
/// 1. Parse JSON to extract base64 string
/// 2. Decode base64 to RLE compressed data
/// 3. Decompress RLE data to final binary
///
/// All operations are done in-place within the provided buffer.
///
/// # Arguments
/// * `json_data` - Mutable buffer containing JSON payload; will be overwritten with decompressed data
///
/// # Returns
/// Tuple of (decompressed_data_length, requires_response) or error message
///
/// # Example
/// ```no_run
/// let mut buffer = b"{\"frame\":\"...\",\"requires_response\":true}".to_vec();
/// let (len, requires_response) = decoding::decode_json_rle_base64(&mut buffer)?;
/// // buffer[..len] now contains the decompressed frame data
/// # Ok::<(), &str>(())
/// ```
pub fn decode_json_rle_base64(json_data: &mut [u8]) -> Result<(usize, bool), &'static str> {
    // Find the end of the JSON data (look for closing brace followed by zeros or end of buffer)
    let json_end = json_data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(json_data.len());

    // Parse JSON to extract base64 string position and response flag
    let (base64_start, base64_len, requires_response) = {
        let json_slice = &json_data[..json_end];
        let parsed: BinaryPayload = serde_json_core::from_slice(json_slice)
            .map_err(|_| "Failed to parse JSON payload")?
            .0;

        // Calculate the position of base64_frame within json_data
        let base64_ptr = parsed.frame.as_ptr();
        let json_ptr = json_data.as_ptr();
        let offset = unsafe { base64_ptr.offset_from(json_ptr) } as usize;

        (offset, parsed.frame.len(), parsed.requires_response)
    };

    // Decode base64 in-place: move base64 string to end, then decode to beginning
    // This avoids overlap issues during decoding
    let decoded_len = {
        // Move base64 data to the end of the buffer to avoid overlap
        let base64_end_offset = json_data.len() - base64_len;
        json_data.copy_within(base64_start..base64_start + base64_len, base64_end_offset);

        // Split buffer to avoid borrow conflicts: decode_dst | base64_src
        let (decode_dst, base64_src) = json_data.split_at_mut(base64_end_offset);

        // Convert base64 source to string
        let base64_str = core::str::from_utf8(base64_src)
            .map_err(|_| "Invalid UTF-8 in base64 data")?;

        // Decode into the destination part (no overlap now)
        base64_decode_inplace(base64_str, decode_dst)?.len()
    };

    // RLE decode in-place: read from end, write to beginning
    let decompressed_len = rle_decode_inplace(json_data, decoded_len);

    Ok((decompressed_len, requires_response))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use super::*;

    #[test]
    fn test_rle_encode_simple() {
        let input = [0xFF, 0xFF, 0xFF, 0x00, 0x00];
        let mut output = [0u8; 10];
        let len = rle_encode(&input, &mut output);

        assert_eq!(len, 4);
        assert_eq!(&output[..len], &[3, 0xFF, 2, 0x00]);
    }

    #[test]
    fn test_rle_encode_no_compression() {
        let input = [0x01, 0x02, 0x03, 0x04];
        let mut output = [0u8; 20];
        let len = rle_encode(&input, &mut output);

        assert_eq!(len, 8);
        assert_eq!(&output[..len], &[1, 0x01, 1, 0x02, 1, 0x03, 1, 0x04]);
    }

    #[test]
    fn test_rle_encode_max_run() {
        let input = [0xAA; 300];
        let mut output = [0u8; 1000];
        let len = rle_encode(&input, &mut output);

        // Should split into 255 + 45
        assert_eq!(len, 4);
        assert_eq!(&output[..len], &[255, 0xAA, 45, 0xAA]);
    }

    #[test]
    fn test_rle_decode_inplace() {
        let mut buffer = [0u8; 100];
        buffer[0] = 3;
        buffer[1] = 0xFF;
        buffer[2] = 2;
        buffer[3] = 0x00;

        let len = rle_decode_inplace(&mut buffer, 4);

        assert_eq!(len, 5);
        assert_eq!(&buffer[..5], &[0xFF, 0xFF, 0xFF, 0x00, 0x00]);
    }

    #[test]
    fn test_rle_roundtrip() {
        let original = [0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01];
        let mut compressed = [0u8; 50];
        let mut decompressed = [0u8; 50];

        // Encode
        let compressed_len = rle_encode(&original, &mut compressed);

        // Copy to decompression buffer
        decompressed[..compressed_len].copy_from_slice(&compressed[..compressed_len]);

        // Decode
        let decompressed_len = rle_decode_inplace(&mut decompressed, compressed_len);

        assert_eq!(decompressed_len, original.len());
        assert_eq!(&decompressed[..decompressed_len], &original);
    }

    #[test]
    fn test_rle_encode_large_pattern() {
        // Simulate e-ink display pattern (lots of white space)
        let mut input = [0xFF; 1000];
        // Add some black pixels
        for i in 100..110 {
            input[i] = 0x00;
        }

        let mut output = [0u8; 2000];
        let len = rle_encode(&input, &mut output);

        // Should be much smaller than original
        assert!(len < 100, "Compressed size {} should be much smaller than 1000", len);
    }

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let mut encoded = [0u8; 100];
        let mut decoded = [0u8; 100];

        // Encode
        let encoded_str = base64_encode(original, &mut encoded).unwrap();

        // Decode
        let decoded_slice = base64_decode_inplace(encoded_str, &mut decoded).unwrap();

        assert_eq!(decoded_slice, original);
    }

    #[test]
    fn test_full_pipeline_rle_then_base64() {
        // Simulate display data
        let original = [0xFF; 100];
        let mut rle_compressed = [0u8; 200];
        let mut base64_encoded = [0u8; 300];
        let mut base64_decoded = [0u8; 200];
        let mut rle_decompressed = [0u8; 200];

        // Step 1: RLE encode
        let rle_len = rle_encode(&original, &mut rle_compressed);
        assert!(rle_len < original.len(), "RLE should compress uniform data");

        // Step 2: Base64 encode
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_encoded).unwrap();

        // Step 3: Base64 decode
        let decoded = base64_decode_inplace(b64_str, &mut base64_decoded).unwrap();
        assert_eq!(decoded.len(), rle_len);

        // Step 4: RLE decode
        rle_decompressed[..decoded.len()].copy_from_slice(decoded);
        let final_len = rle_decode_inplace(&mut rle_decompressed, decoded.len());

        assert_eq!(final_len, original.len());
        assert_eq!(&rle_decompressed[..final_len], &original);
    }

    #[test]
    fn test_decode_json_rle_base64_simple() {
        // Create test data
        let original = [0xFF, 0xFF, 0xFF, 0x00, 0x00];

        // Encode: RLE -> base64
        let mut rle_compressed = [0u8; 20];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let mut base64_buf = [0u8; 50];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

        // Expected base64 for [3, 0xFF, 2, 0x00] is "A/8CAA=="
        assert_eq!(b64_str, "A/8CAA==");

        // Create JSON payload - use hardcoded version to test
        let json = b"{\"frame\":\"A/8CAA==\",\"requires_response\":true}";
        let mut json_bytes = vec![0u8; json.len() + 100]; // Extra space for decompression
        json_bytes[..json.len()].copy_from_slice(json);

        // Decode
        let (len, requires_response) = decode_json_rle_base64(&mut json_bytes).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&json_bytes[..len], &original);
        assert_eq!(requires_response, true);
    }

    #[test]
    fn test_decode_json_rle_base64_large() {
        // Simulate e-ink display data (mostly white with some black)
        let mut original = [0xFF; 1000];
        for i in 100..110 {
            original[i] = 0x00;
        }

        // Encode: RLE -> base64
        let mut rle_compressed = [0u8; 2000];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let mut base64_buf = [0u8; 3000];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

        // Create JSON payload
        let json_string = std::format!(r#"{{"frame":"{}","requires_response":false}}"#, b64_str);
        let json = json_string.as_bytes();
        let mut json_bytes = vec![0u8; json.len() + 3000];
        json_bytes[..json.len()].copy_from_slice(json);

        // Decode
        let (len, requires_response) = decode_json_rle_base64(&mut json_bytes).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&json_bytes[..len], &original[..]);
        assert_eq!(requires_response, false);
    }

    #[test]
    fn test_decode_json_invalid_json() {
        let mut json_bytes = b"{invalid json}".to_vec();
        let result = decode_json_rle_base64(&mut json_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_json_invalid_base64() {
        let json = b"{\"frame\":\"!!!invalid_base64!!!\",\"requires_response\":true}";
        let mut json_bytes = vec![0u8; 500];
        json_bytes[..json.len()].copy_from_slice(json);

        let result = decode_json_rle_base64(&mut json_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_payload_parsing() {
        let json = b"{\"frame\":\"AQID\",\"requires_response\":true}";
        let parsed: BinaryPayload = serde_json_core::from_slice(json).unwrap().0;
        assert_eq!(parsed.frame, "AQID");
        assert_eq!(parsed.requires_response, true);
    }

    #[test]
    fn test_binary_payload_parsing_with_padding() {
        // Test with extra zeros at the end - need to slice to avoid TrailingCharacters error
        let json = b"{\"frame\":\"A/8CAA==\",\"requires_response\":true}";
        let mut json_bytes = vec![0u8; json.len() + 100];
        json_bytes[..json.len()].copy_from_slice(json);

        // Find end of JSON (first zero or end of buffer)
        let json_end = json_bytes.iter().position(|&b| b == 0).unwrap_or(json_bytes.len());

        let parsed: BinaryPayload = serde_json_core::from_slice(&json_bytes[..json_end]).unwrap().0;
        assert_eq!(parsed.frame, "A/8CAA==");
        assert_eq!(parsed.requires_response, true);
    }
}
