//! Crate for encoding and decoding MQTT display data
//! Provides RLE compression and base64 encoding/decoding for e-ink display frames
//! Data is compressed using RLE encoding, then base64 encoded for transmission
#![no_std]
#![deny(missing_docs)]

use base64ct::{Base64, Encoding};
use serde::Deserialize;


/// JSON payload structure for raw binary display data
/// Expected format: {"frame": "base64_encoded_string", "requires_response": true/false, "chunk_index": 0, "total_chunks": 1}
/// The frame data is base64-encoded RLE compressed data chunk
/// RLE format: [count, byte, count, byte, ...] where count is u8 (1-255)
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct BinaryPayload<'a> {
    #[serde(borrow)]
    /// Base64-encoded RLE compressed frame data chunk
    pub frame: &'a str,
    /// Whether the client requires a response
    pub requires_response: bool,
    /// Index of this chunk (0-based)
    pub chunk_index: usize,
    /// Total number of chunks
    pub total_chunks: usize,
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
        while i + (count as usize) < data.len() && data[i + (count as usize)] == byte && count < 255
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

/// RLE decoder - decodes from input to output buffer
/// Format: [count, byte, count, byte, ...]
///
/// # Arguments
/// * `input` - Compressed data
/// * `output` - Output buffer for decompressed data
///
/// # Returns
/// Number of decompressed bytes
///
/// # Example
/// ```
/// let compressed = [3, 0xFF, 2, 0x00];
/// let mut output = [0u8; 100];
/// let len = decoding::rle_decode(&compressed, &mut output);
/// assert_eq!(len, 5);
/// assert_eq!(&output[..5], &[0xFF, 0xFF, 0xFF, 0x00, 0x00]);
/// ```
pub fn rle_decode(input: &[u8], output: &mut [u8]) -> usize {
    let mut read_pos = 0;
    let mut write_pos = 0;

    while read_pos + 1 < input.len() {
        let count = input[read_pos] as usize;
        let byte = input[read_pos + 1];

        for _ in 0..count {
            if write_pos < output.len() {
                output[write_pos] = byte;
                write_pos += 1;
            }
        }
        read_pos += 2;
    }

    write_pos
}

/// Decode base64 string into a buffer
///
/// # Arguments
/// * `base64_str` - Base64 encoded string
/// * `output` - Output buffer for decoded data
///
/// # Returns
/// Decoded bytes or error message
pub fn base64_decode<'a>(
    base64_str: &str,
    output: &'a mut [u8],
) -> Result<&'a [u8], &'static str> {
    Base64::decode(base64_str, output).map_err(|_| "Failed to decode base64 data")
}

/// Encode data to base64 string
///
/// # Arguments
/// * `data` - Input data to encode
/// * `output` - Output buffer for base64 string
///
/// # Returns
/// Base64 encoded string or error
pub fn base64_encode<'a>(data: &[u8], output: &'a mut [u8]) -> Result<&'a str, &'static str> {
    Base64::encode(data, output).map_err(|_| "Failed to encode base64 data")
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

/// Parse chunk metadata from JSON payload without decoding the frame data
///
/// # Arguments
/// * `json_data` - Buffer containing JSON payload
///
/// # Returns
/// ChunkMetadata or error message
pub fn parse_chunk_metadata(json_data: &[u8]) -> Result<ChunkMetadata, &'static str> {
    let json_end = json_data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(json_data.len());

    let json_slice = &json_data[..json_end];
    let parsed: BinaryPayload = serde_json_core::from_slice(json_slice)
        .map_err(|_| "Failed to parse JSON payload")?
        .0;

    Ok(ChunkMetadata {
        requires_response: parsed.requires_response,
        chunk_index: parsed.chunk_index,
        total_chunks: parsed.total_chunks,
    })
}

/// Decode a single chunk from JSON payload containing base64-encoded RLE data
///
/// Decodes: JSON → base64 → RLE → raw binary
///
/// # Arguments
/// * `json_data` - Buffer containing JSON payload; will be overwritten with decompressed data
///
/// # Returns
/// Tuple of (decompressed_length, chunk_metadata) or error
pub fn decode_chunk(json_data: &mut [u8]) -> Result<(usize, ChunkMetadata), &'static str> {
    // Parse JSON and extract metadata + base64 position
    let json_end = json_data.iter().position(|&b| b == 0).unwrap_or(json_data.len());
    let (b64_start, b64_len, metadata) = {
        let parsed: BinaryPayload = serde_json_core::from_slice(&json_data[..json_end])
            .map_err(|_| "Failed to parse JSON")?
            .0;

        // Calculate base64 string position within buffer
        let b64_ptr = parsed.frame.as_ptr();
        let buf_ptr = json_data.as_ptr();
        let offset = unsafe { b64_ptr.offset_from(buf_ptr) } as usize;

        (
            offset,
            parsed.frame.len(),
            ChunkMetadata {
                requires_response: parsed.requires_response,
                chunk_index: parsed.chunk_index,
                total_chunks: parsed.total_chunks,
            },
        )
    };

    // Move base64 to end of buffer to make room for decoding
    let rle_start = json_data.len() - b64_len;
    json_data.copy_within(b64_start..b64_start + b64_len, rle_start);

    // Decode base64 (from end) to RLE data (at beginning)
    let (rle_dst, b64_src) = json_data.split_at_mut(rle_start);
    let b64_str = core::str::from_utf8(b64_src).map_err(|_| "Invalid UTF-8")?;
    let rle_data = Base64::decode(b64_str, rle_dst).map_err(|_| "Failed to decode base64")?;
    let rle_len = rle_data.len();

    // Move RLE data to end, then decode to beginning
    let rle_src_start = json_data.len() - rle_len;
    json_data.copy_within(0..rle_len, rle_src_start);

    let (output, rle_src) = json_data.split_at_mut(rle_src_start);
    let decompressed_len = rle_decode(rle_src, output);

    Ok((decompressed_len, metadata))
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

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
    fn test_rle_decode() {
        let compressed = [3, 0xFF, 2, 0x00];
        let mut output = [0u8; 100];

        let len = rle_decode(&compressed, &mut output);

        assert_eq!(len, 5);
        assert_eq!(&output[..5], &[0xFF, 0xFF, 0xFF, 0x00, 0x00]);
    }

    #[test]
    fn test_rle_roundtrip() {
        let original = [0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01];
        let mut compressed = [0u8; 50];
        let mut decompressed = [0u8; 50];

        // Encode
        let compressed_len = rle_encode(&original, &mut compressed);

        // Decode
        let decompressed_len = rle_decode(&compressed[..compressed_len], &mut decompressed);

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
        assert!(
            len < 100,
            "Compressed size {} should be much smaller than 1000",
            len
        );
    }

    #[test]
    fn test_base64_roundtrip() {
        let original = b"Hello, World!";
        let mut encoded = [0u8; 100];
        let mut decoded = [0u8; 100];

        // Encode
        let encoded_str = base64_encode(original, &mut encoded).unwrap();

        // Decode
        let decoded_slice = base64_decode(encoded_str, &mut decoded).unwrap();

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
        let decoded = base64_decode(b64_str, &mut base64_decoded).unwrap();
        assert_eq!(decoded.len(), rle_len);

        // Step 4: RLE decode
        let final_len = rle_decode(decoded, &mut rle_decompressed);

        assert_eq!(final_len, original.len());
        assert_eq!(&rle_decompressed[..final_len], &original);
    }

    #[test]
    fn test_decode_chunk_simple() {
        // Create test data
        let original = [0xFF, 0xFF, 0xFF, 0x00, 0x00];

        // Encode: RLE -> base64
        let mut rle_compressed = [0u8; 20];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let mut base64_buf = [0u8; 50];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

        // Expected base64 for [3, 0xFF, 2, 0x00] is "A/8CAA=="
        assert_eq!(b64_str, "A/8CAA==");

        // Create JSON payload with chunk metadata
        let json = b"{\"frame\":\"A/8CAA==\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let mut json_bytes = vec![0u8; json.len() + 100]; // Extra space for decompression
        json_bytes[..json.len()].copy_from_slice(json);

        // Decode
        let (len, metadata) = decode_chunk(&mut json_bytes).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&json_bytes[..len], &original);
        assert_eq!(metadata.requires_response, true);
        assert_eq!(metadata.chunk_index, 0);
        assert_eq!(metadata.total_chunks, 1);
    }

    #[test]
    fn test_decode_chunk_large() {
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

        // Create JSON payload with chunk metadata
        let json_string = std::format!(r#"{{"frame":"{}","requires_response":false,"chunk_index":0,"total_chunks":1}}"#, b64_str);
        let json = json_string.as_bytes();
        let mut json_bytes = vec![0u8; json.len() + 3000];
        json_bytes[..json.len()].copy_from_slice(json);

        // Decode
        let (len, metadata) = decode_chunk(&mut json_bytes).unwrap();

        assert_eq!(len, original.len());
        assert_eq!(&json_bytes[..len], &original[..]);
        assert_eq!(metadata.requires_response, false);
        assert_eq!(metadata.chunk_index, 0);
        assert_eq!(metadata.total_chunks, 1);
    }

    #[test]
    fn test_decode_chunk_invalid_json() {
        let mut json_bytes = b"{invalid json}".to_vec();
        let result = decode_chunk(&mut json_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_chunk_invalid_base64() {
        let json = b"{\"frame\":\"!!!invalid_base64!!!\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let mut json_bytes = vec![0u8; 500];
        json_bytes[..json.len()].copy_from_slice(json);

        let result = decode_chunk(&mut json_bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_payload_parsing() {
        let json = b"{\"frame\":\"AQID\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let parsed: BinaryPayload = serde_json_core::from_slice(json).unwrap().0;
        assert_eq!(parsed.frame, "AQID");
        assert_eq!(parsed.requires_response, true);
        assert_eq!(parsed.chunk_index, 0);
        assert_eq!(parsed.total_chunks, 1);
    }

    #[test]
    fn test_binary_payload_parsing_with_padding() {
        // Test with extra zeros at the end - need to slice to avoid TrailingCharacters error
        let json = b"{\"frame\":\"A/8CAA==\",\"requires_response\":true,\"chunk_index\":0,\"total_chunks\":1}";
        let mut json_bytes = vec![0u8; json.len() + 100];
        json_bytes[..json.len()].copy_from_slice(json);

        // Find end of JSON (first zero or end of buffer)
        let json_end = json_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(json_bytes.len());

        let parsed: BinaryPayload = serde_json_core::from_slice(&json_bytes[..json_end])
            .unwrap()
            .0;
        assert_eq!(parsed.frame, "A/8CAA==");
        assert_eq!(parsed.requires_response, true);
        assert_eq!(parsed.chunk_index, 0);
        assert_eq!(parsed.total_chunks, 1);
    }

    #[test]
    fn test_e2e_realistic_15kb_display_data() {
        // Simulate realistic 400x300 e-ink display data (15,000 bytes)
        // Display size: 400 pixels wide × 300 pixels tall = 120,000 pixels
        // At 1 bit per pixel: 120,000 / 8 = 15,000 bytes
        const DISPLAY_SIZE: usize = 15_000;
        let mut original = [0u8; DISPLAY_SIZE];

        // IMPORTANT: Buffer size requirements for in-place decoding:
        // - Minimum buffer size must be >= DISPLAY_SIZE (15,000 bytes)
        // - The JSON payload is much smaller (~3KB) but expands to 15KB during decoding
        // - Recommended: 15,360 bytes (15KB + 360 byte safety margin)

        // Create realistic display pattern:
        // - White background (0xFF = all pixels white)
        // - Some text/graphics regions (0x00 = all pixels black)
        // - Grayscale patterns (mixed bytes)

        // Fill with white background
        original.fill(0xFF);

        // Add some "text" regions (horizontal bars of black pixels)
        for row in [10, 30, 50, 70, 90, 110, 130, 150, 170, 190] {
            let start = row * 50; // 50 bytes per simulated row
            let end = start + 40; // 40 bytes of black
            if end < DISPLAY_SIZE {
                original[start..end].fill(0x00);
            }
        }

        // Add some grayscale patterns (simulating anti-aliased text or images)
        for i in (5000..5500).step_by(2) {
            if i < DISPLAY_SIZE {
                original[i] = 0xAA; // 10101010 pattern
            }
        }
        for i in (10000..10500).step_by(2) {
            if i < DISPLAY_SIZE {
                original[i] = 0x55; // 01010101 pattern
            }
        }

        std::println!("Original display data size: {} bytes", original.len());

        // Step 1: RLE compress the data
        let mut rle_compressed = vec![0u8; DISPLAY_SIZE * 2]; // Worst case: 2x size if no compression
        let rle_len = rle_encode(&original, &mut rle_compressed);
        rle_compressed.truncate(rle_len);

        std::println!(
            "RLE compressed size: {} bytes ({:.1}% of original)",
            rle_len,
            (rle_len as f64 / original.len() as f64) * 100.0
        );

        // Verify compression achieved <7KB target
        assert!(
            rle_len < 7000,
            "RLE compression should reduce 15KB to <7KB, got {} bytes",
            rle_len
        );

        // Step 2: Base64 encode the compressed data
        let base64_max_size = ((rle_len + 2) / 3) * 4; // Base64 expands by ~33%
        let mut base64_buf = vec![0u8; base64_max_size + 100];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf)
            .expect("Base64 encoding should succeed");

        std::println!("Base64 encoded size: {} bytes", b64_str.len());

        // Step 3: Create JSON payload (BinaryPayload format) as single chunk
        let json_string = std::format!(r#"{{"frame":"{}","requires_response":true,"chunk_index":0,"total_chunks":1}}"#, b64_str);
        std::println!("JSON payload size: {} bytes", json_string.len());

        // Allocate buffer with extra space for in-place decoding
        let mut json_bytes = vec![0u8; json_string.len() + DISPLAY_SIZE + 1000];
        json_bytes[..json_string.len()].copy_from_slice(json_string.as_bytes());

        // Step 4: Decode using the full pipeline (simulates real-world usage)
        let (decompressed_len, metadata) =
            decode_chunk(&mut json_bytes).expect("Decoding should succeed");

        std::println!("Decompressed size: {} bytes", decompressed_len);

        // Step 5: Verify results
        assert_eq!(
            decompressed_len, DISPLAY_SIZE,
            "Decompressed data should be {} bytes",
            DISPLAY_SIZE
        );
        assert_eq!(metadata.requires_response, true, "requires_response should be true");
        assert_eq!(metadata.chunk_index, 0);
        assert_eq!(metadata.total_chunks, 1);
        assert_eq!(
            &json_bytes[..decompressed_len],
            &original[..],
            "Decompressed data should match original exactly"
        );

        std::println!(
            "✓ E2E test passed: 15KB → {}KB (compressed) → 15KB (decompressed)",
            rle_len / 1000
        );
    }

    #[test]
    fn test_compression_realistic_text_document() {
        // Test realistic text document with typical text density (~30% coverage)
        const DISPLAY_SIZE: usize = 15_000;
        let mut original = [0u8; DISPLAY_SIZE];

        // White background
        original.fill(0xFF);

        // Add realistic text coverage - alternating text/whitespace
        // Simulate 30 lines of text with realistic character patterns
        for line in 0..30 {
            let line_start = line * 500; // ~50 bytes per line
            for char_block in 0..8 {
                let start = line_start + char_block * 60;
                let end = start + 35; // ~35 bytes of text per character block
                if end < DISPLAY_SIZE {
                    // Varied patterns for different characters
                    for i in start..end {
                        original[i] = if (i - start) % 7 < 4 { 0x00 } else { 0xFF };
                    }
                }
            }
        }

        let mut rle_compressed = vec![0u8; DISPLAY_SIZE * 2];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let compression_ratio = (rle_len as f64 / DISPLAY_SIZE as f64) * 100.0;
        std::println!(
            "Text document compression: {:.1}% ({} bytes)",
            compression_ratio,
            rle_len
        );

        // Text documents should compress to 30-50% with RLE
        assert!(
            rle_len < (DISPLAY_SIZE as f64 * 0.6) as usize,
            "Text should compress to <60%, got {:.1}%",
            compression_ratio
        );
    }

    #[test]
    fn test_compression_worst_case_photo() {
        // Worst case: photo/image with lots of dithering (checkerboard-like patterns)
        const DISPLAY_SIZE: usize = 15_000;
        let mut original = [0u8; DISPLAY_SIZE];

        // Simulate dithered image - alternating patterns (worst case for RLE)
        for i in 0..DISPLAY_SIZE {
            // Create pseudo-random dithering pattern
            original[i] = match i % 7 {
                0 => 0xFF,
                1 => 0xAA,
                2 => 0x55,
                3 => 0x00,
                4 => 0xCC,
                5 => 0x33,
                _ => 0x0F,
            };
        }

        let mut rle_compressed = vec![0u8; DISPLAY_SIZE * 2];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let compression_ratio = (rle_len as f64 / DISPLAY_SIZE as f64) * 100.0;
        std::println!(
            "Dithered image (worst case) compression: {:.1}% ({} bytes)",
            compression_ratio,
            rle_len
        );

        // Dithered images might not compress well, could even expand to ~200%
        // This is expected for RLE on highly varied data
        std::println!("⚠️  Note: RLE is not suitable for dithered images");
    }

    #[test]
    fn test_compression_qr_code() {
        // QR code: lots of small blocks with some repetition
        const DISPLAY_SIZE: usize = 15_000;
        let mut original = [0u8; DISPLAY_SIZE];

        // White background
        original.fill(0xFF);

        // Simulate QR code pattern (200x200 pixels centered)
        // QR codes have ~50% black/white ratio with blocks
        let qr_start_row = 50; // Center vertically
        let qr_rows = 25; // 200 pixels / 8 bits

        for row in 0..qr_rows {
            let row_start = (qr_start_row + row) * 50;
            // Simulate QR code blocks (3x3 pixel blocks)
            for block in 0..20 {
                let block_start = row_start + 5 + block * 2;
                let is_black_block = (row + block) % 3 != 0;
                if block_start + 1 < DISPLAY_SIZE {
                    original[block_start] = if is_black_block { 0x00 } else { 0xFF };
                    original[block_start + 1] = if is_black_block { 0x00 } else { 0xFF };
                }
            }
        }

        let mut rle_compressed = vec![0u8; DISPLAY_SIZE * 2];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let compression_ratio = (rle_len as f64 / DISPLAY_SIZE as f64) * 100.0;
        std::println!(
            "QR code compression: {:.1}% ({} bytes)",
            compression_ratio,
            rle_len
        );

        // QR codes should compress reasonably well (20-40%)
        assert!(
            rle_len < (DISPLAY_SIZE as f64 * 0.5) as usize,
            "QR code should compress to <50%, got {:.1}%",
            compression_ratio
        );
    }

    #[test]
    fn test_compression_simple_graphics() {
        // Simple graphics: charts, diagrams (lots of solid regions)
        const DISPLAY_SIZE: usize = 15_000;
        let mut original = [0u8; DISPLAY_SIZE];

        // White background
        original.fill(0xFF);

        // Add some solid bars (like a bar chart)
        for bar in 0..10 {
            let bar_start = 1000 + bar * 1200;
            let bar_height = 200 + bar * 50;
            for i in bar_start..bar_start + bar_height {
                if i < DISPLAY_SIZE {
                    original[i] = 0x00;
                }
            }
        }

        let mut rle_compressed = vec![0u8; DISPLAY_SIZE * 2];
        let rle_len = rle_encode(&original, &mut rle_compressed);

        let compression_ratio = (rle_len as f64 / DISPLAY_SIZE as f64) * 100.0;
        std::println!(
            "Simple graphics compression: {:.1}% ({} bytes)",
            compression_ratio,
            rle_len
        );

        // Simple graphics should compress very well (<20%)
        assert!(
            rle_len < (DISPLAY_SIZE as f64 * 0.25) as usize,
            "Simple graphics should compress to <25%, got {:.1}%",
            compression_ratio
        );
    }

    #[test]
    fn test_multi_chunk_reassembly() {
        // Test encoding and decoding data split into multiple chunks
        // Each chunk contains uncompressed data that gets RLE-encoded independently
        const CHUNK_SIZE: usize = 500; // Uncompressed chunk size
        let original = [0xFF; 1500]; // Original data

        let num_chunks = (original.len() + CHUNK_SIZE - 1) / CHUNK_SIZE;
        std::println!(
            "Splitting {} bytes into {} chunks of ~{} bytes each",
            original.len(),
            num_chunks,
            CHUNK_SIZE
        );

        let mut reassembled = vec![0u8; original.len()];
        let mut offset = 0;

        for chunk_idx in 0..num_chunks {
            let chunk_start = chunk_idx * CHUNK_SIZE;
            let chunk_end = core::cmp::min(chunk_start + CHUNK_SIZE, original.len());
            let chunk_data = &original[chunk_start..chunk_end];

            // RLE encode this chunk
            let mut rle_compressed = vec![0u8; chunk_data.len() * 2];
            let rle_len = rle_encode(chunk_data, &mut rle_compressed);

            // Base64 encode the RLE-compressed chunk
            let mut base64_buf = vec![0u8; rle_len * 2];
            let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

            // Create JSON payload for this chunk
            let is_last_chunk = chunk_idx == num_chunks - 1;
            let json_string = std::format!(
                r#"{{"frame":"{}","requires_response":{},"chunk_index":{},"total_chunks":{}}}"#,
                b64_str, is_last_chunk, chunk_idx, num_chunks
            );

            std::println!(
                "Chunk {}/{}: {} bytes uncompressed → {} bytes RLE → {} bytes base64 (JSON: {} bytes)",
                chunk_idx + 1,
                num_chunks,
                chunk_data.len(),
                rle_len,
                b64_str.len(),
                json_string.len()
            );

            // Decode chunk
            let mut json_bytes = vec![0u8; json_string.len() + 2000];
            json_bytes[..json_string.len()].copy_from_slice(json_string.as_bytes());

            let (decoded_len, metadata) = decode_chunk(&mut json_bytes).unwrap();

            // Verify metadata
            assert_eq!(metadata.chunk_index, chunk_idx);
            assert_eq!(metadata.total_chunks, num_chunks);
            assert_eq!(metadata.requires_response, is_last_chunk);

            // Verify decoded chunk size matches original chunk size
            assert_eq!(decoded_len, chunk_data.len());

            // Copy decoded chunk to reassembly buffer
            reassembled[offset..offset + decoded_len].copy_from_slice(&json_bytes[..decoded_len]);
            offset += decoded_len;
        }

        // Verify reassembled data matches original
        assert_eq!(offset, original.len());
        assert_eq!(&reassembled[..offset], &original[..]);

        std::println!("✓ Multi-chunk reassembly test passed");
    }

    #[test]
    fn test_parse_chunk_metadata() {
        // Test parsing chunk metadata without decoding
        let json = b"{\"frame\":\"AQID\",\"requires_response\":true,\"chunk_index\":2,\"total_chunks\":5}";

        let metadata = parse_chunk_metadata(json).unwrap();

        assert_eq!(metadata.requires_response, true);
        assert_eq!(metadata.chunk_index, 2);
        assert_eq!(metadata.total_chunks, 5);
    }

}
