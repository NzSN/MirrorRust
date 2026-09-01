use crate::Error;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_PROTOCOL_LINE_BYTES: usize = 65_535;

pub fn validate_protocol_line(line: &str) -> Result<(), Error> {
    if line.is_empty() {
        return Err(Error::InvalidArgument(
            "protocol line must not be empty".into(),
        ));
    }
    if line.contains('\n') {
        return Err(Error::InvalidArgument(
            "protocol line contains an embedded newline".into(),
        ));
    }
    if line.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(Error::InvalidArgument(
            "protocol line exceeds 65535-byte UTF-8 payload limit".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct TlsOptions {
    pub ca_path: PathBuf,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub pin: Option<String>,
    pub server_name: Option<String>,
    pub handshake_timeout: Duration,
}

impl TlsOptions {
    pub fn new(
        ca_path: impl Into<PathBuf>,
        cert_path: impl Into<PathBuf>,
        key_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            ca_path: ca_path.into(),
            cert_path: cert_path.into(),
            key_path: key_path.into(),
            pin: None,
            server_name: None,
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

enum TransportInner {
    Stdio {
        child: Child,
        stdin: Option<ChildStdin>,
        reader: BufReader<ChildStdout>,
    },
    Tcp(BufReader<TcpStream>),
    Tls(Box<BufReader<TlsStream>>),
}

/// A single Mirrors protocol session over stdio, TCP, or TLS 1.3 mTLS.
pub struct Transport {
    inner: TransportInner,
    closed: bool,
    close_code: Option<i32>,
    peer_fingerprint: Option<String>,
    async_capable: bool,
}

pub fn spawn_mirror(bin_path: &str) -> Result<Transport, Error> {
    let mut child = Command::new(bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Io(std::io::Error::other("failed to capture child stdin")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Io(std::io::Error::other("failed to capture child stdout")))?;
    Ok(Transport {
        inner: TransportInner::Stdio {
            child,
            stdin: Some(stdin),
            reader: BufReader::new(stdout),
        },
        closed: false,
        close_code: None,
        peer_fingerprint: None,
        async_capable: false,
    })
}

/// Connect to `mirror --serve` over plain TCP.
pub fn connect_mirror(host: &str, port: u16) -> Result<Transport, Error> {
    let socket = TcpStream::connect((host, port))?;
    socket.set_nodelay(true)?;
    Ok(Transport {
        inner: TransportInner::Tcp(BufReader::new(socket)),
        closed: false,
        close_code: None,
        peer_fingerprint: None,
        async_capable: true,
    })
}

/// Connect to `mirror --server --tls` using TLS 1.3 and a client certificate.
///
/// rustls/webpki verifies the requested DNS name or IP address against SAN;
/// it does not fall back to the certificate subject CN. IP identities are
/// represented as `ServerName::IpAddress`, for which rustls omits SNI.
pub fn connect_tls_mirror(host: &str, port: u16, options: &TlsOptions) -> Result<Transport, Error> {
    assert_private_key_mode(&options.key_path)?;

    let ca_certs = load_certificates(&options.ca_path, "CA")?;
    let mut roots = RootCertStore::empty();
    for cert in ca_certs {
        roots
            .add(cert)
            .map_err(|e| Error::Tls(format!("invalid CA certificate: {e}")))?;
    }
    if roots.is_empty() {
        return Err(Error::Tls("CA file contains no certificates".into()));
    }

    let client_certs = load_certificates(&options.cert_path, "client")?;
    if client_certs.is_empty() {
        return Err(Error::Tls(
            "client certificate file contains no certificates".into(),
        ));
    }
    let client_key = load_private_key(&options.key_path)?;
    let config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_client_auth_cert(client_certs, client_key)
        .map_err(|e| Error::Tls(format!("invalid client certificate/key: {e}")))?;

    let verify_name = options.server_name.as_deref().unwrap_or(host);
    let server_name = ServerName::try_from(verify_name.to_string())
        .map_err(|_| Error::InvalidArgument(format!("invalid TLS server name: {verify_name}")))?;
    let mut connection = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| Error::Tls(format!("cannot create TLS client: {e}")))?;
    let mut socket = TcpStream::connect((host, port))?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(options.handshake_timeout))?;
    socket.set_write_timeout(Some(options.handshake_timeout))?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut socket)
            .map_err(|e| Error::Tls(format!("TLS handshake failed: {e}")))?;
    }
    socket.set_read_timeout(None)?;
    socket.set_write_timeout(None)?;

    let leaf = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .ok_or_else(|| Error::Tls("TLS peer did not present a certificate".into()))?;
    let fingerprint = sha256_hex(leaf.as_ref());
    if let Some(pin) = &options.pin {
        let expected = normalize_fingerprint(pin)?;
        if fingerprint != expected {
            return Err(Error::Tls(format!(
                "certificate fingerprint mismatch: expected {expected}, got {fingerprint}"
            )));
        }
    }

    Ok(Transport {
        inner: TransportInner::Tls(Box::new(BufReader::new(StreamOwned::new(
            connection, socket,
        )))),
        closed: false,
        close_code: None,
        peer_fingerprint: Some(fingerprint),
        async_capable: true,
    })
}

impl Transport {
    /// Write a single newline-terminated line and flush.
    pub fn send(&mut self, line: &str) -> Result<(), Error> {
        validate_protocol_line(line)?;
        if self.closed {
            return Err(Error::TransportClosed);
        }
        match &mut self.inner {
            TransportInner::Stdio { stdin, .. } => {
                let writer = stdin.as_mut().ok_or(Error::TransportClosed)?;
                write_protocol_line(writer, line)
            }
            TransportInner::Tcp(reader) => write_protocol_line(reader.get_mut(), line),
            TransportInner::Tls(reader) => write_protocol_line(reader.get_mut(), line),
        }
    }

    /// Read one strict newline-terminated protocol line. `Ok(None)` on clean EOF.
    pub fn recv(&mut self) -> Result<Option<String>, Error> {
        if self.closed {
            return Err(Error::TransportClosed);
        }
        match &mut self.inner {
            TransportInner::Stdio { reader, .. } => read_protocol_line(reader),
            TransportInner::Tcp(reader) => read_protocol_line(reader),
            TransportInner::Tls(reader) => read_protocol_line(reader),
        }
    }

    /// Close the transport. Network transports return zero; stdio returns the
    /// child process exit code. The operation is idempotent.
    pub fn close(&mut self) -> Result<i32, Error> {
        if let Some(code) = self.close_code {
            return Ok(code);
        }
        self.closed = true;
        let code = match &mut self.inner {
            TransportInner::Stdio { child, stdin, .. } => {
                stdin.take();
                child.wait()?.code().unwrap_or(0)
            }
            TransportInner::Tcp(reader) => {
                shutdown_socket(reader.get_mut())?;
                0
            }
            TransportInner::Tls(reader) => {
                let stream = reader.get_mut();
                stream.conn.send_close_notify();
                let _ = stream.flush();
                shutdown_socket(&stream.sock)?;
                0
            }
        };
        self.close_code = Some(code);
        Ok(code)
    }

    pub fn is_async_capable(&self) -> bool {
        self.async_capable
    }

    /// SHA-256 of the peer leaf certificate DER, lowercase hexadecimal.
    pub fn peer_fingerprint(&self) -> Option<&str> {
        self.peer_fingerprint.as_deref()
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn write_protocol_line(writer: &mut impl Write, line: &str) -> Result<(), Error> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn shutdown_socket(socket: &TcpStream) -> Result<(), Error> {
    match socket.shutdown(Shutdown::Both) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_protocol_line(reader: &mut impl BufRead) -> Result<Option<String>, Error> {
    let mut bytes = Vec::new();
    let read = reader
        .take((MAX_PROTOCOL_LINE_BYTES + 2) as u64)
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unterminated or oversized protocol line",
        )));
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "protocol line exceeds 65535-byte UTF-8 payload limit",
        )));
    }
    String::from_utf8(bytes).map(Some).map_err(|e| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("protocol line is not UTF-8: {e}"),
        ))
    })
}

fn load_certificates(path: &Path, label: &str) -> Result<Vec<CertificateDer<'static>>, Error> {
    let file = File::open(path)?;
    rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Error::Tls(format!("cannot parse {label} certificate file: {e}")))
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, Error> {
    let file = File::open(path)?;
    rustls_pemfile::private_key(&mut BufReader::new(file))?
        .ok_or_else(|| Error::Tls("client key file contains no private key".into()))
}

#[cfg(unix)]
fn assert_private_key_mode(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(Error::InvalidArgument(format!(
            "client key {} is accessible by group/other; chmod 0600 is required",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn assert_private_key_mode(_path: &Path) -> Result<(), Error> {
    Ok(())
}

fn normalize_fingerprint(pin: &str) -> Result<String, Error> {
    let normalized = pin
        .trim()
        .chars()
        .filter(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::InvalidArgument(
            "certificate pin must be 64 hexadecimal SHA-256 characters".into(),
        ));
    }
    Ok(normalized)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
