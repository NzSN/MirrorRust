use crate::{connect_tls_mirror, Error, TlsOptions, Transport};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorServiceInfo {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub cert_sha256: Option<String>,
}

struct HttpUrl {
    host: String,
    port: u16,
    prefix: String,
}

fn parse_http_url(url: &str) -> Option<HttpUrl> {
    let rest = url.strip_prefix("http://")?;
    let (authority, raw_prefix) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => (host, port.parse::<u16>().ok()?),
        _ => (authority, 80),
    };
    let prefix = raw_prefix.trim_matches('/');
    Some(HttpUrl {
        host: host.to_string(),
        port,
        prefix: if prefix.is_empty() {
            String::new()
        } else {
            format!("/{prefix}")
        },
    })
}

fn http_get(url: &HttpUrl) -> Option<Vec<u8>> {
    let mut socket = TcpStream::connect((url.host.as_str(), url.port)).ok()?;
    let path = format!("{}/v1/health/service/modelmirrors", url.prefix);
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        url.host, url.port
    )
    .ok()?;
    socket.flush().ok()?;
    let mut response = Vec::new();
    socket.read_to_end(&mut response).ok()?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let headers = std::str::from_utf8(&response[..split]).ok()?;
    let status = headers.lines().next()?;
    if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
        return None;
    }
    Some(response[split + 4..].to_vec())
}

fn normalize_pin(pin: &str) -> Option<String> {
    let normalized = pin.trim().to_ascii_lowercase();
    (normalized.len() == 64 && normalized.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some(normalized)
}

fn parse_entry(value: &Value) -> Option<MirrorServiceInfo> {
    let service = value.get("Service")?.as_object()?;
    let host = service.get("Address")?.as_str()?.trim();
    if host.is_empty() {
        return None;
    }
    let port = service.get("Port")?.as_u64()?;
    let port = u16::try_from(port).ok().filter(|port| *port != 0)?;
    let cert_sha256 = match service
        .get("Meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("cert-sha256"))
    {
        None | Some(Value::Null) => None,
        Some(Value::String(pin)) => Some(normalize_pin(pin)?),
        Some(_) => return None,
    };
    Some(MirrorServiceInfo {
        id: service
            .get("ID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        host: host.to_string(),
        port,
        cert_sha256,
    })
}

/// Discover healthy Mirrors endpoints from a Consul-compatible registry.
/// Registry failures are deliberately fail-closed and return no candidates.
pub fn discover_mirrors(registry_url: &str) -> Vec<MirrorServiceInfo> {
    let Some(url) = parse_http_url(registry_url) else {
        return Vec::new();
    };
    let Some(body) = http_get(&url) else {
        return Vec::new();
    };
    let Ok(Value::Array(entries)) = serde_json::from_slice(&body) else {
        return Vec::new();
    };
    entries.iter().filter_map(parse_entry).collect()
}

/// Discover registry candidates and connect to the first usable mTLS peer.
/// A caller-supplied pin overrides per-entry metadata; otherwise each
/// candidate's `cert-sha256` value is enforced when present.
pub fn connect_mirror_from_registry(
    registry_url: &str,
    tls: &TlsOptions,
    pin_override: Option<&str>,
) -> Result<Transport, Error> {
    let candidates = discover_mirrors(registry_url);
    if candidates.is_empty() {
        return Err(Error::Registry("no usable mirror candidates".into()));
    }
    let mut failures = Vec::new();
    for candidate in candidates {
        let mut options = tls.clone();
        options.pin = pin_override
            .map(str::to_string)
            .or_else(|| candidate.cert_sha256.clone());
        match connect_tls_mirror(&candidate.host, candidate.port, &options) {
            Ok(transport) => return Ok(transport),
            Err(error) => failures.push(format!(
                "{}({}:{}): {}",
                candidate.id, candidate.host, candidate.port, error
            )),
        }
    }
    Err(Error::Registry(format!(
        "all mirror candidates failed: {}",
        failures.join("; ")
    )))
}
