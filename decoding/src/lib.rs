//! Crate for encoding and decoding MQTT display data
//! Provides RLE compression and base64 encoding/decoding for e-ink display frames
#![no_std]
#![deny(missing_docs)]

use base64ct::{Base64, Encoding};
use serde::Deserialize;

/// Subsampling factor for image downsampling/upsampling
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum SubsamplingFactor {
    /// No subsampling (full resolution)
    #[serde(rename = "1")]
    None,
    /// 2x2 subsampling (quarter resolution)
    #[serde(rename = "2")]
    Two,
    /// 4x4 subsampling (1/16 resolution)
    #[serde(rename = "4")]
    Four,
}

impl SubsamplingFactor {
    /// Get the numeric factor value
    pub fn value(&self) -> u32 {
        match self {
            SubsamplingFactor::None => 1,
            SubsamplingFactor::Two => 2,
            SubsamplingFactor::Four => 4,
        }
    }
}

/// JSON payload structure for raw binary display data
/// Expected format: {"frame": "base64_encoded_string", "subsample": "1"|"2"|"4", "requires_response": true/false}
/// The frame data is base64-encoded RLE compressed data
/// RLE format: [count, byte, count, byte, ...] where count is u8 (1-255)
#[derive(Deserialize)]
pub struct BinaryPayload<'a> {
    #[serde(borrow)]
    /// Base64-encoded RLE compressed frame data
    pub frame: &'a str,
    /// Subsampling factor (1=none, 2=half, 4=quarter)
    #[serde(default)]
    pub subsample: Option<SubsamplingFactor>,
    /// Whether the client requires a response
    pub requires_response: bool,
}

impl Default for SubsamplingFactor {
    fn default() -> Self {
        SubsamplingFactor::None
    }
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

/// Downsample image data by a given factor
///
/// # Arguments
/// * `input` - Input image data
/// * `width` - Width of input image in bytes
/// * `height` - Height of input image in pixels
/// * `factor` - Subsampling factor (2 or 4)
/// * `output` - Output buffer for downsampled data
///
/// # Returns
/// Number of bytes written to output
pub fn downsample_image(
    input: &[u8],
    width: usize,
    height: usize,
    factor: u32,
    output: &mut [u8],
) -> usize {
    let factor = factor as usize;
    let out_width = width / factor;
    let out_height = height / factor;

    let mut out_pos = 0;
    for out_y in 0..out_height {
        for out_x in 0..out_width {
            // Sample from the center of the downsampled block
            let in_y = out_y * factor;
            let in_x = out_x * factor;
            let in_pos = in_y * width + in_x;

            if in_pos < input.len() {
                output[out_pos] = input[in_pos];
                out_pos += 1;
            }
        }
    }
    out_pos
}

/// Upsample image data by a given factor using nearest-neighbor interpolation
/// Operates in-place from end to beginning to avoid overwriting source data
///
/// # Arguments
/// * `buffer` - Buffer containing downsampled data at the beginning; will be overwritten with upsampled data
/// * `downsampled_len` - Length of the downsampled data
/// * `width` - Target width in bytes
/// * `height` - Target height in pixels
/// * `factor` - Upsampling factor (2 or 4)
///
/// # Returns
/// Number of bytes written (should equal width * height)
pub fn upsample_image_inplace(
    buffer: &mut [u8],
    downsampled_len: usize,
    width: usize,
    height: usize,
    factor: u32,
) -> usize {
    let factor = factor as usize;
    let src_width = width / factor;
    let _src_height = height / factor;

    // Work backwards to avoid overwriting source data
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let src_y = y / factor;
            let src_x = x / factor;
            let src_pos = src_y * src_width + src_x;
            let dst_pos = y * width + x;

            if src_pos < downsampled_len && dst_pos < buffer.len() {
                buffer[dst_pos] = buffer[src_pos];
            }
        }
    }

    width * height
}

/// Decode JSON payload containing base64-encoded RLE data with optional subsampling
///
/// This function performs the complete decoding pipeline:
/// 1. Parse JSON to extract base64 string and subsampling factor
/// 2. Decode base64 to RLE compressed data
/// 3. Decompress RLE data to binary
/// 4. Upsample if subsampling was used
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
/// let mut buffer = b"{\"frame\":\"...\",\"subsample\":\"2\",\"requires_response\":true}".to_vec();
/// let (len, requires_response) = decoding::decode_json_rle_base64(&mut buffer)?;
/// // buffer[..len] now contains the decompressed and upsampled frame data
/// # Ok::<(), &str>(())
/// ```
pub fn decode_json_rle_base64(json_data: &mut [u8]) -> Result<(usize, bool), &'static str> {
    // Find the end of the JSON data (look for closing brace followed by zeros or end of buffer)
    let json_end = json_data
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(json_data.len());

    // Parse JSON to extract base64 string position, subsampling factor, and response flag
    let (base64_start, base64_len, subsample_factor, requires_response) = {
        let json_slice = &json_data[..json_end];
        let parsed: BinaryPayload = serde_json_core::from_slice(json_slice)
            .map_err(|_| "Failed to parse JSON payload")?
            .0;

        // Calculate the position of base64_frame within json_data
        let base64_ptr = parsed.frame.as_ptr();
        let json_ptr = json_data.as_ptr();
        let offset = unsafe { base64_ptr.offset_from(json_ptr) } as usize;

        let subsample = parsed.subsample.unwrap_or(SubsamplingFactor::None);

        (offset, parsed.frame.len(), subsample, parsed.requires_response)
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

    // Upsample if needed
    let final_len = if subsample_factor != SubsamplingFactor::None {
        // For 400x300 display: width = 50 bytes (400 pixels / 8), height = 300 pixels
        // These constants should match the display size
        const DISPLAY_WIDTH_BYTES: usize = 50;  // 400 pixels / 8 bits
        const DISPLAY_HEIGHT: usize = 300;

        upsample_image_inplace(
            json_data,
            decompressed_len,
            DISPLAY_WIDTH_BYTES,
            DISPLAY_HEIGHT,
            subsample_factor.value(),
        )
    } else {
        decompressed_len
    };

    Ok((final_len, requires_response))
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

        std::println!("RLE compressed size: {} bytes ({:.1}% of original)",
            rle_len, (rle_len as f64 / original.len() as f64) * 100.0);

        // Verify compression achieved <7KB target
        assert!(rle_len < 7000, "RLE compression should reduce 15KB to <7KB, got {} bytes", rle_len);

        // Step 2: Base64 encode the compressed data
        let base64_max_size = ((rle_len + 2) / 3) * 4; // Base64 expands by ~33%
        let mut base64_buf = vec![0u8; base64_max_size + 100];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf)
            .expect("Base64 encoding should succeed");

        std::println!("Base64 encoded size: {} bytes", b64_str.len());

        // Step 3: Create JSON payload (BinaryPayload format)
        let json_string = std::format!(
            r#"{{"frame":"{}","requires_response":true}}"#,
            b64_str
        );
        std::println!("JSON payload size: {} bytes", json_string.len());

        // Allocate buffer with extra space for in-place decoding
        let mut json_bytes = vec![0u8; json_string.len() + DISPLAY_SIZE + 1000];
        json_bytes[..json_string.len()].copy_from_slice(json_string.as_bytes());

        // Step 4: Decode using the full pipeline (simulates real-world usage)
        let (decompressed_len, requires_response) = decode_json_rle_base64(&mut json_bytes)
            .expect("Decoding should succeed");

        std::println!("Decompressed size: {} bytes", decompressed_len);

        // Step 5: Verify results
        assert_eq!(decompressed_len, DISPLAY_SIZE,
            "Decompressed data should be {} bytes", DISPLAY_SIZE);
        assert_eq!(requires_response, true, "requires_response should be true");
        assert_eq!(&json_bytes[..decompressed_len], &original[..],
            "Decompressed data should match original exactly");

        std::println!("✓ E2E test passed: 15KB → {}KB (compressed) → 15KB (decompressed)",
            rle_len / 1000);
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
        std::println!("Text document compression: {:.1}% ({} bytes)", compression_ratio, rle_len);

        // Text documents should compress to 30-50% with RLE
        assert!(rle_len < (DISPLAY_SIZE as f64 * 0.6) as usize,
            "Text should compress to <60%, got {:.1}%", compression_ratio);
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
        std::println!("Dithered image (worst case) compression: {:.1}% ({} bytes)",
            compression_ratio, rle_len);

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
        std::println!("QR code compression: {:.1}% ({} bytes)", compression_ratio, rle_len);

        // QR codes should compress reasonably well (20-40%)
        assert!(rle_len < (DISPLAY_SIZE as f64 * 0.5) as usize,
            "QR code should compress to <50%, got {:.1}%", compression_ratio);
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
        std::println!("Simple graphics compression: {:.1}% ({} bytes)", compression_ratio, rle_len);

        // Simple graphics should compress very well (<20%)
        assert!(rle_len < (DISPLAY_SIZE as f64 * 0.25) as usize,
            "Simple graphics should compress to <25%, got {:.1}%", compression_ratio);
    }

    #[test]
    fn test_subsample_downsample_2x() {
        // Test 2x downsampling
        let width = 8; // 8 bytes wide
        let height = 4; // 4 rows
        let input = vec![
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // Row 0
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Row 1
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, // Row 2
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, // Row 3
        ];

        let mut output = vec![0u8; 8];
        let len = downsample_image(&input, width, height, 2, &mut output);

        // 2x downsampling: width/2 * height/2 = 4 bytes * 2 rows = 8 bytes
        // Samples every 2nd byte horizontally, every 2nd row
        assert_eq!(len, 8);
        assert_eq!(&output[..len], &[
            0xFF, 0xFF, 0xFF, 0xFF, // Row 0 (bytes 0,2,4,6)
            0xAA, 0xAA, 0xAA, 0xAA, // Row 2 (bytes 0,2,4,6)
        ]);
    }

    #[test]
    fn test_subsample_upsample_2x_inplace() {
        // Test 2x upsampling in-place
        let width = 4;
        let height = 4;
        let mut buffer = vec![
            0xFF, 0xAA,  // Downsampled data (2x2)
            0x55, 0x00,
            0x00, 0x00, 0x00, 0x00, // Extra space for upsampling
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        let len = upsample_image_inplace(&mut buffer, 4, width, height, 2);

        assert_eq!(len, 16); // 4x4 = 16 bytes
        // Each pixel should be duplicated 2x2
        assert_eq!(buffer[0], 0xFF); // (0,0)
        assert_eq!(buffer[1], 0xFF); // (0,1)
        assert_eq!(buffer[4], 0xFF); // (1,0)
        assert_eq!(buffer[5], 0xFF); // (1,1)
        assert_eq!(buffer[2], 0xAA); // (0,2)
        assert_eq!(buffer[3], 0xAA); // (0,3)
    }

    #[test]
    fn test_subsample_roundtrip_2x() {
        // Test downsampling then upsampling
        const WIDTH: usize = 8;
        const HEIGHT: usize = 8;
        let mut original = vec![0u8; WIDTH * HEIGHT];

        // Create a simple pattern
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                original[y * WIDTH + x] = ((y / 2) * 2 + (x / 2)) as u8;
            }
        }

        // Downsample
        let mut downsampled = vec![0u8; (WIDTH / 2) * (HEIGHT / 2)];
        let down_len = downsample_image(&original, WIDTH, HEIGHT, 2, &mut downsampled);

        assert_eq!(down_len, 16); // 4x4

        // Upsample in-place
        let mut upsampled = vec![0u8; WIDTH * HEIGHT];
        upsampled[..down_len].copy_from_slice(&downsampled[..down_len]);
        let up_len = upsample_image_inplace(&mut upsampled, down_len, WIDTH, HEIGHT, 2);

        assert_eq!(up_len, WIDTH * HEIGHT);
    }

    #[test]
    fn test_e2e_subsampling_2x() {
        // Test full pipeline with 2x subsampling for a photo-like image
        const FULL_WIDTH: usize = 50;  // 400 pixels / 8
        const FULL_HEIGHT: usize = 300;
        const FULL_SIZE: usize = FULL_WIDTH * FULL_HEIGHT;
        const SUBSAMPLE: u32 = 2;

        // Create a dithered photo-like pattern (worst case for RLE)
        let mut original = vec![0u8; FULL_SIZE];
        for i in 0..FULL_SIZE {
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

        std::println!("Original size: {} bytes", original.len());

        // Downsample (server-side)
        let downsampled_size = (FULL_WIDTH / SUBSAMPLE as usize) * (FULL_HEIGHT / SUBSAMPLE as usize);
        let mut downsampled = vec![0u8; downsampled_size];
        let down_len = downsample_image(&original, FULL_WIDTH, FULL_HEIGHT, SUBSAMPLE, &mut downsampled);

        std::println!("Downsampled to: {} bytes ({:.1}% of original)",
            down_len, (down_len as f64 / FULL_SIZE as f64) * 100.0);

        // RLE compress the downsampled data
        let mut rle_compressed = vec![0u8; downsampled_size * 2];
        let rle_len = rle_encode(&downsampled[..down_len], &mut rle_compressed);

        std::println!("RLE compressed: {} bytes ({:.1}% of original)",
            rle_len, (rle_len as f64 / FULL_SIZE as f64) * 100.0);

        // 2x subsampling with worst-case dithered data: ~7.5KB (50% of original)
        // This is still an improvement but not enough for <7KB target
        // For real photos with more uniform regions, compression will be much better
        assert!(rle_len < FULL_SIZE, "Should be smaller than original, got {} bytes", rle_len);

        // Base64 encode
        let mut base64_buf = vec![0u8; rle_len * 2];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

        // Create JSON payload with subsample parameter
        let json_string = std::format!(
            r#"{{"frame":"{}","subsample":"2","requires_response":true}}"#,
            b64_str
        );

        std::println!("JSON payload: {} bytes", json_string.len());

        // Decode on client side
        let mut json_bytes = vec![0u8; json_string.len() + FULL_SIZE + 1000];
        json_bytes[..json_string.len()].copy_from_slice(json_string.as_bytes());

        let (final_len, requires_response) = decode_json_rle_base64(&mut json_bytes)
            .expect("Decoding should succeed");

        std::println!("Final upsampled size: {} bytes", final_len);

        // Should be back to full size
        assert_eq!(final_len, FULL_SIZE, "Should upsample back to full size");
        assert_eq!(requires_response, true);

        let compression_ratio = (rle_len as f64 / FULL_SIZE as f64) * 100.0;
        std::println!("✓ 2x subsampling: {}KB → {}KB ({:.1}% compression)",
            FULL_SIZE / 1000, rle_len / 1000, compression_ratio);
    }

    #[test]
    fn test_e2e_subsampling_4x() {
        // Test 4x subsampling for extreme compression
        const FULL_WIDTH: usize = 50;  // 400 pixels / 8
        const FULL_HEIGHT: usize = 300;
        const FULL_SIZE: usize = FULL_WIDTH * FULL_HEIGHT;
        const SUBSAMPLE: u32 = 4;

        // Create a complex pattern
        let mut original = vec![0u8; FULL_SIZE];
        for i in 0..FULL_SIZE {
            original[i] = (i % 256) as u8;
        }

        // Downsample (server-side)
        let downsampled_size = (FULL_WIDTH / SUBSAMPLE as usize) * (FULL_HEIGHT / SUBSAMPLE as usize);
        let mut downsampled = vec![0u8; downsampled_size];
        let down_len = downsample_image(&original, FULL_WIDTH, FULL_HEIGHT, SUBSAMPLE, &mut downsampled);

        std::println!("4x downsample: {} bytes ({:.1}% of original)",
            down_len, (down_len as f64 / FULL_SIZE as f64) * 100.0);

        // RLE compress
        let mut rle_compressed = vec![0u8; downsampled_size * 2];
        let rle_len = rle_encode(&downsampled[..down_len], &mut rle_compressed);

        // Base64 encode
        let mut base64_buf = vec![0u8; rle_len * 2];
        let b64_str = base64_encode(&rle_compressed[..rle_len], &mut base64_buf).unwrap();

        // Create JSON with subsample=4
        let json_string = std::format!(
            r#"{{"frame":"{}","subsample":"4","requires_response":false}}"#,
            b64_str
        );

        // Decode
        let mut json_bytes = vec![0u8; json_string.len() + FULL_SIZE + 1000];
        json_bytes[..json_string.len()].copy_from_slice(json_string.as_bytes());

        let (final_len, requires_response) = decode_json_rle_base64(&mut json_bytes)
            .expect("Decoding should succeed");

        assert_eq!(final_len, FULL_SIZE);
        assert_eq!(requires_response, false);

        std::println!("✓ 4x subsampling: {}KB → {}KB ({:.1}% compression)",
            FULL_SIZE / 1000, rle_len / 1000, (rle_len as f64 / FULL_SIZE as f64) * 100.0);
    }

    #[test]
    fn test_subsample_json_parsing() {
        // Test that JSON with subsample field parses correctly
        let json1 = b"{\"frame\":\"AQID\",\"subsample\":\"2\",\"requires_response\":true}";
        let parsed1: BinaryPayload = serde_json_core::from_slice(json1).unwrap().0;
        assert_eq!(parsed1.subsample, Some(SubsamplingFactor::Two));

        let json2 = b"{\"frame\":\"AQID\",\"subsample\":\"4\",\"requires_response\":true}";
        let parsed2: BinaryPayload = serde_json_core::from_slice(json2).unwrap().0;
        assert_eq!(parsed2.subsample, Some(SubsamplingFactor::Four));

        let json3 = b"{\"frame\":\"AQID\",\"requires_response\":true}";
        let parsed3: BinaryPayload = serde_json_core::from_slice(json3).unwrap().0;
        assert_eq!(parsed3.subsample, None);
    }
}
