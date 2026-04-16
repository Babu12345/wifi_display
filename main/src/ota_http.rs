//! HTTPS firmware download for OTA updates
//!
//! Performs a bare HTTP/1.1 GET over TLS using the existing `esp-mbedtls`
//! stack. Uses its own TCP socket so the MQTT connection stays open.

use core::ffi::CStr;
use core::fmt::Write;

use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::Duration;
use embedded_io_async::Write as AsyncWrite;
use esp_mbedtls::{Certificates, Mode, TlsVersion, X509, asynch::Session};
use heapless::String;

use crate::ota_flash::EspFlashWriter;

/// Timeout for the OTA TCP socket (2 minutes for large downloads)
const OTA_SOCKET_TIMEOUT_SECS: u64 = 120;

/// TCP buffer size for OTA downloads (smaller than MQTT since we only do GET)
const OTA_TCP_BUFFER_SIZE: usize = 4096;

// Static buffers for OTA TCP socket to avoid stack overflow
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};

static OTA_TCP_RX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; OTA_TCP_BUFFER_SIZE]> =
    Mutex::new([0u8; OTA_TCP_BUFFER_SIZE]);
static OTA_TCP_TX_BUFFER: Mutex<CriticalSectionRawMutex, [u8; OTA_TCP_BUFFER_SIZE]> =
    Mutex::new([0u8; OTA_TCP_BUFFER_SIZE]);

/// Perform the full OTA download and flash write.
///
/// Opens a separate HTTPS connection to the firmware URL, streams the
/// response body in chunks, and writes each chunk to flash via `OtaManager`.
///
/// Returns the version string on success (for the ACK message).
pub async fn download_and_flash<'a>(
    stack: &Stack<'static>,
    tls: &'a esp_mbedtls::Tls<'a>,
    ca_cert_pem: &'a [u8],
    mgr: &mut ota::OtaManager<EspFlashWriter>,
    trigger_payload: &'a [u8],
) -> Result<&'a str, &'static str> {
    // Step 1: Parse trigger and prepare flash
    let trigger = mgr
        .begin_update(trigger_payload)
        .map_err(|_| "Failed to parse OTA trigger")?;

    log::info!(
        "OTA: downloading version={} size={} from {}",
        trigger.version,
        trigger.size,
        trigger.url
    );

    // Step 2: Parse URL
    let url = ota::url::parse_https_url(trigger.url).map_err(|e| {
        log::error!("OTA URL parse error: {}", e);
        "Invalid OTA URL"
    })?;

    // Step 3: DNS lookup
    let fw_ip = stack
        .dns_query(url.host, embassy_net::dns::DnsQueryType::A)
        .await
        .map_err(|_| "OTA DNS lookup failed")?
        .first()
        .ok_or("No IP for OTA host")?
        .clone();

    log::info!("OTA: resolved {} to {}", url.host, fw_ip);

    // Step 4: TCP connect (using separate static buffers)
    let mut rx_buffer = OTA_TCP_RX_BUFFER.lock().await;
    let mut tx_buffer = OTA_TCP_TX_BUFFER.lock().await;
    let mut socket = TcpSocket::new(stack.clone(), &mut *rx_buffer, &mut *tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(OTA_SOCKET_TIMEOUT_SECS)));

    socket
        .connect((fw_ip, url.port))
        .await
        .map_err(|_| "OTA TCP connect failed")?;

    log::info!("OTA: TCP connected to {}:{}", url.host, url.port);

    // Step 5: TLS handshake (server-only verification, no client cert)
    // Build null-terminated servername for TLS SNI
    let mut servername_buf = [0u8; 128];
    let servername_len = url.host.len().min(126);
    servername_buf[..servername_len].copy_from_slice(&url.host.as_bytes()[..servername_len]);
    servername_buf[servername_len] = 0;
    let servername = CStr::from_bytes_with_nul(&servername_buf[..servername_len + 1])
        .map_err(|_| "Invalid servername")?;

    let certificates = Certificates {
        ca_chain: X509::pem(ca_cert_pem).ok(),
        certificate: None,
        private_key: None,
        password: None,
    };

    let mut session = Session::new(
        &mut socket,
        Mode::Client { servername },
        TlsVersion::Tls1_2,
        certificates,
        tls.reference(),
    )
    .inspect_err(|e| log::error!("OTA TLS session creation error: {:?}", e))
    .map_err(|_| "OTA TLS session creation failed")?;

    session
        .connect()
        .await
        .inspect_err(|e| log::error!("OTA TLS handshake error: {:?}", e))
        .map_err(|_| "OTA TLS handshake failed")?;

    log::info!("OTA: TLS connected");

    // Step 6: Send HTTP GET request
    let mut request = String::<512>::new();
    write!(
        &mut request,
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        url.path, url.host
    )
    .map_err(|_| "OTA request too long")?;

    session
        .write_all(request.as_bytes())
        .await
        .map_err(|_| "OTA request send failed")?;

    log::info!("OTA: GET request sent");

    // Step 7: Read and skip HTTP response headers
    // Read until we find "\r\n\r\n" marking end of headers
    let mut header_buf = [0u8; 1024];
    let mut header_len = 0;
    let header_end;

    loop {
        if header_len >= header_buf.len() {
            return Err("OTA response headers too large");
        }
        let n = session
            .read(&mut header_buf[header_len..])
            .await
            .map_err(|_| "OTA header read failed")?;
        if n == 0 {
            return Err("OTA connection closed during headers");
        }
        header_len += n;

        // Search for end of headers
        if let Some(pos) = find_header_end(&header_buf[..header_len]) {
            header_end = pos + 4; // skip past \r\n\r\n
            break;
        }
    }

    // Check for HTTP 200 status
    let header_str = core::str::from_utf8(&header_buf[..header_end]).unwrap_or("");
    if !header_str.starts_with("HTTP/1.1 200") && !header_str.starts_with("HTTP/1.0 200") {
        log::error!(
            "OTA: unexpected response: {}",
            &header_str[..header_str.len().min(40)]
        );
        return Err("OTA server returned non-200 status");
    }

    log::info!("OTA: HTTP 200 OK, starting download");

    // Step 8: Write any body bytes that were in the header buffer
    let leftover = header_len - header_end;
    if leftover > 0 {
        mgr.write_chunk(&header_buf[header_end..header_len])
            .map_err(|_| "OTA flash write failed")?;
    }

    // Step 9: Stream remaining body chunks to flash
    let mut chunk_buf = [0u8; ota::DEFAULT_CHUNK_SIZE];
    loop {
        let n = session
            .read(&mut chunk_buf)
            .await
            .map_err(|_| "OTA body read failed")?;
        if n == 0 {
            break;
        }

        mgr.write_chunk(&chunk_buf[..n])
            .map_err(|_| "OTA flash write failed")?;

        // Log progress periodically (every ~64KB)
        let written = mgr.progress().bytes_written;
        if written % (64 * 1024) < n as u32 {
            log::info!(
                "OTA: {}% ({}/{} bytes)",
                mgr.progress().percent(),
                written,
                trigger.size
            );
        }
    }

    log::info!(
        "OTA: download complete, {} bytes written",
        mgr.progress().bytes_written
    );

    // Step 10: Finalize (size + CRC verify + set boot target)
    mgr.finalize_update(trigger.version).map_err(|e| {
        log::error!("OTA finalize failed: {:?}", e);
        "OTA finalize failed"
    })?;

    log::info!("OTA: firmware verified and boot target set");

    Ok(trigger.version)
}

/// Find the "\r\n\r\n" sequence that marks end of HTTP headers
fn find_header_end(data: &[u8]) -> Option<usize> {
    if data.len() < 4 {
        return None;
    }
    for i in 0..data.len() - 3 {
        if &data[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
}
