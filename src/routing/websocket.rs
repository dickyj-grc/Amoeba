//! WebSocket pass-through. After the proxy has authorized the caller and woken
//! the app, the upgrade handshake is sent to the app over a plain TCP
//! connection; when the app answers `101 Switching Protocols`, the caller gets
//! that answer and the two connections are joined byte for byte until either
//! side closes. Frames are not parsed, so any WebSocket subprotocol works,
//! including the Chrome DevTools Protocol that Playwright and Puppeteer use.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, warn};

/// The app must answer the handshake within this time.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest handshake response head accepted from the app.
const MAX_RESPONSE_HEAD: usize = 64 * 1024;

/// A WebSocket handshake: `Connection` lists `upgrade` and `Upgrade` is
/// `websocket` (both case-insensitive, as RFC 6455 requires).
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let connection_upgrade = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    let upgrade_websocket = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"));
    connection_upgrade && upgrade_websocket
}

/// Sends the handshake to `host:port` with `headers` (already filtered and
/// rewritten by the proxy; `Host`, `Connection` and `Upgrade` are set here),
/// and answers the caller with the app's response. On `101` the connections
/// are joined in a background task that holds `on_close` until both sides are
/// done; drop that to mark the session over.
pub async fn tunnel<G: Send + 'static>(
    host: &str,
    port: u16,
    path_and_query: &str,
    host_header: &str,
    headers: &HeaderMap,
    client: OnUpgrade,
    on_close: G,
) -> Result<Response, String> {
    let mut upstream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("connecting to {host}:{port} timed out"))?
        .map_err(|e| format!("cannot connect to {host}:{port}: {e}"))?;

    upstream
        .write_all(&handshake_request(path_and_query, host_header, headers))
        .await
        .map_err(|e| format!("sending the handshake failed: {e}"))?;

    let (status, response_headers, leftover) =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_response_head(&mut upstream))
            .await
            .map_err(|_| "the app did not answer the WebSocket handshake in time".to_string())??;

    let mut builder = Response::builder().status(status);
    for (name, value) in &response_headers {
        builder = builder.header(name, value);
    }

    if status != StatusCode::SWITCHING_PROTOCOLS {
        // The app refused the upgrade: pass its answer on as-is. The body is
        // whatever arrived with the head; the connection is closed after it.
        return builder
            .body(Body::from(leftover))
            .map_err(|e| format!("invalid response from the app: {e}"));
    }

    tokio::spawn(async move {
        let _session = on_close;
        let upgraded = match client.await {
            Ok(upgraded) => upgraded,
            Err(e) => {
                warn!("the caller's WebSocket upgrade failed: {e}");
                return;
            }
        };
        let mut caller = TokioIo::new(upgraded);
        // Bytes the app sent right after its 101 belong to the caller.
        if !leftover.is_empty() && caller.write_all(&leftover).await.is_err() {
            return;
        }
        match tokio::io::copy_bidirectional(&mut caller, &mut upstream).await {
            Ok((sent, received)) => debug!(sent, received, "WebSocket session closed"),
            Err(e) => debug!("WebSocket session ended: {e}"),
        }
    });

    builder
        .body(Body::empty())
        .map_err(|e| format!("invalid response from the app: {e}"))
}

/// The HTTP/1.1 upgrade request for the app.
fn handshake_request(path_and_query: &str, host_header: &str, headers: &HeaderMap) -> Vec<u8> {
    let mut request = format!("GET {path_and_query} HTTP/1.1\r\nHost: {host_header}\r\n");
    request.push_str("Connection: Upgrade\r\nUpgrade: websocket\r\n");
    for (name, value) in headers {
        if matches!(
            *name,
            header::HOST | header::CONNECTION | header::UPGRADE | header::CONTENT_LENGTH
        ) {
            continue;
        }
        // Header values may be any bytes except CR/LF, which HeaderValue rejects.
        request.push_str(name.as_str());
        request.push_str(": ");
        request.push_str(&String::from_utf8_lossy(value.as_bytes()));
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.into_bytes()
}

type ResponseHead = (StatusCode, Vec<(header::HeaderName, HeaderValue)>, Vec<u8>);

/// Reads the app's response head. Returns the status, its headers and any
/// bytes read past the head.
async fn read_response_head(upstream: &mut TcpStream) -> Result<ResponseHead, String> {
    let mut buf = Vec::with_capacity(1024);
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let n = upstream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("reading the handshake answer failed: {e}"))?;
        if n == 0 {
            return Err("the app closed the connection during the WebSocket handshake".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_RESPONSE_HEAD {
            return Err("the app's handshake answer is too large".into());
        }
    };

    let mut parsed_headers = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut parsed_headers);
    response
        .parse(&buf[..head_end])
        .map_err(|e| format!("the app's handshake answer is not valid HTTP: {e}"))?;
    let status = response
        .code
        .and_then(|code| StatusCode::from_u16(code).ok())
        .ok_or("the app's handshake answer has no status")?;
    let headers = response
        .headers
        .iter()
        .filter_map(|h| {
            Some((
                header::HeaderName::from_bytes(h.name.as_bytes()).ok()?,
                HeaderValue::from_bytes(h.value).ok()?,
            ))
        })
        .collect();
    Ok((status, headers, buf[head_end..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn recognizes_websocket_handshakes_only() {
        assert!(is_websocket_upgrade(&headers(&[
            ("connection", "keep-alive, Upgrade"),
            ("upgrade", "WebSocket"),
        ])));
        assert!(!is_websocket_upgrade(&headers(&[("upgrade", "websocket")])));
        assert!(!is_websocket_upgrade(&headers(&[
            ("connection", "upgrade"),
            ("upgrade", "h2c"),
        ])));
        assert!(!is_websocket_upgrade(&headers(&[])));
    }

    #[test]
    fn handshake_sets_host_and_upgrade_once() {
        let request = handshake_request(
            "/devtools/browser/1?x=1",
            "localhost:9222",
            &headers(&[
                ("host", "evil.example"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
                ("x-amoeba-user", "alice"),
            ]),
        );
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with(
            "GET /devtools/browser/1?x=1 HTTP/1.1\r\nHost: localhost:9222\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
        ));
        assert!(!text.contains("evil.example"));
        assert!(text.contains("sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(text.contains("x-amoeba-user: alice\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
