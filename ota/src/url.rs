//! Minimal URL parser for HTTPS firmware URLs
//!
//! Extracts host, port, and path from URLs like:
//! `https://example.com/firmware/1.0.0/firmware-signed.bin`
//!
//! Only supports HTTPS (port 443 by default).

/// Parsed URL components
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedUrl<'a> {
    /// Hostname (e.g., "example.com")
    pub host: &'a str,
    /// Port number (defaults to 443 for HTTPS)
    pub port: u16,
    /// Path including leading slash (e.g., "/firmware/1.0.0/fw.bin")
    pub path: &'a str,
}

/// Parse an HTTPS URL into its components.
///
/// Only `https://` URLs are accepted. Port defaults to 443.
pub fn parse_https_url(url: &str) -> Result<ParsedUrl<'_>, &'static str> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("URL must start with https://")?;

    if rest.is_empty() {
        return Err("URL has no host");
    }

    // Split host+port from path
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    // Split host from port
    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let port_str = &host_port[i + 1..];
            let port = parse_port(port_str).ok_or("Invalid port number")?;
            (&host_port[..i], port)
        }
        None => (host_port, 443),
    };

    if host.is_empty() {
        return Err("URL has empty host");
    }

    Ok(ParsedUrl { host, port, path })
}

/// Parse a port string into a u16 (no_std compatible)
fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || s.len() > 5 {
        return None;
    }
    let mut result: u32 = 0;
    for &b in s.as_bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        result = result * 10 + (b - b'0') as u32;
    }
    if result > 65535 {
        return None;
    }
    Some(result as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_url() {
        let url = parse_https_url("https://example.com/firmware/1.0.0/fw.bin").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.path, "/firmware/1.0.0/fw.bin");
    }

    #[test]
    fn test_url_with_port() {
        let url = parse_https_url("https://example.com:8443/fw.bin").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 8443);
        assert_eq!(url.path, "/fw.bin");
    }

    #[test]
    fn test_url_no_path() {
        let url = parse_https_url("https://example.com").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 443);
        assert_eq!(url.path, "/");
    }

    #[test]
    fn test_url_root_path() {
        let url = parse_https_url("https://example.com/").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.path, "/");
    }

    #[test]
    fn test_github_pages_url() {
        let url = parse_https_url(
            "https://babu12345.github.io/portrait_v2/firmware/1.0.0/firmware-signed.bin",
        )
        .unwrap();
        assert_eq!(url.host, "babu12345.github.io");
        assert_eq!(url.port, 443);
        assert_eq!(
            url.path,
            "/portrait_v2/firmware/1.0.0/firmware-signed.bin"
        );
    }

    #[test]
    fn test_s3_presigned_url() {
        let url = parse_https_url(
            "https://my-bucket.s3.amazonaws.com/firmware/v1.2.0/fw.bin",
        )
        .unwrap();
        assert_eq!(url.host, "my-bucket.s3.amazonaws.com");
        assert_eq!(url.path, "/firmware/v1.2.0/fw.bin");
    }

    #[test]
    fn test_http_rejected() {
        assert!(parse_https_url("http://example.com/fw.bin").is_err());
    }

    #[test]
    fn test_empty_url() {
        assert!(parse_https_url("").is_err());
    }

    #[test]
    fn test_no_host() {
        assert!(parse_https_url("https://").is_err());
    }

    #[test]
    fn test_invalid_port() {
        assert!(parse_https_url("https://example.com:abc/fw.bin").is_err());
    }

    #[test]
    fn test_port_too_large() {
        assert!(parse_https_url("https://example.com:99999/fw.bin").is_err());
    }
}
