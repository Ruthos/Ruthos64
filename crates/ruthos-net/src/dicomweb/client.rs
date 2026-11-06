//! The HTTP side of a DICOMweb node: its rules, its TLS and its refusals.
//!
//! A gateway is treated as untrusted input, like a CD or a C-STORE from an
//! unknown modality. Redirects are followed only within the same host over
//! HTTPS, and a reply is read with a limit whatever it claims about itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use reqwest::{redirect, RequestBuilder, Response, StatusCode, Url};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use super::auth::SignIn;
use crate::NetError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Nothing received for this long is a stall. A whole study has no limit.
pub const STALL: Duration = Duration::from_secs(120);
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// Which transfer syntax to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transfer {
    /// As stored: `*`.
    #[default]
    Original,
    ExplicitLittleEndian,
    Jpeg2000Lossless,
}

impl Transfer {
    pub fn parameter(self) -> &'static str {
        match self {
            Transfer::Original => "*",
            Transfer::ExplicitLittleEndian => "1.2.840.10008.1.2.1",
            Transfer::Jpeg2000Lossless => "1.2.840.10008.1.2.4.90",
        }
    }
}

/// A DICOMweb node, ready to use.
#[derive(Clone)]
pub struct WebNode {
    pub name: String,
    /// The service roots, without a trailing slash.
    pub qido: String,
    pub wado: String,
    pub stow: String,
    pub sign_in: Arc<dyn SignIn>,
    /// SHA-256 of the gateway's public key, replacing chain validation.
    pub pin: Option<[u8; 32]>,
    /// Certificate and key as PEM, for mutual TLS.
    pub client_certificate: Option<Vec<u8>>,
    /// Instead of the system's proxy.
    pub proxy: Option<String>,
    pub transfer: Transfer,
    pub page_size: u32,
    /// `includefield=all` works here. `None` until a test finds out.
    pub include_all: Option<bool>,
    /// Series retrieved at once, one to eight.
    pub parallel: usize,
    /// Parts accepted in one reply.
    pub most_parts: usize,
}

/// True for this computer. Hosts are compared whole, so that
/// `localhost.example.org` is not taken for it.
pub fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost") | Some("127.0.0.1"))
}

/// The rules for a node's address. Returns it without a trailing slash. A
/// password in clear text would expose the account and the studies behind it,
/// and a node with no sign-in must not be reachable by anyone else.
pub fn check_address(address: &str, signs_in: bool) -> Result<String, NetError> {
    let text = address.trim().trim_end_matches('/');
    let url = Url::parse(text).map_err(|_| NetError::Insecure(format!("\"{text}\" is not an address")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NetError::Insecure(format!("\"{text}\" is not an HTTPS address")));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetError::Insecure(
            "put the user name and password under sign-in, not in the address".into(),
        ));
    }
    if url.scheme() != "https" && !is_local(&url) {
        return Err(NetError::Insecure("HTTP is only allowed on this computer".into()));
    }
    if !signs_in && !is_local(&url) {
        return Err(NetError::Insecure(
            "a node with no sign-in must be on this computer".into(),
        ));
    }
    Ok(text.to_string())
}

/// A pin as SHA-256 of the public key: hex, with or without colons, or base64.
pub fn parse_pin(text: &str) -> Option<[u8; 32]> {
    use base64::Engine;
    let text = text.trim();
    let text = text.strip_prefix("sha256/").unwrap_or(text);
    let hex: String = text.chars().filter(|c| *c != ':').collect();
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        return Some(out);
    }
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()?
        .try_into()
        .ok()
}

/// One DER element: its whole length, and where its content starts.
fn der_element(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.get(1)?;
    let (length, head) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for b in bytes.get(2..2 + count)? {
            length = length << 8 | *b as usize;
        }
        (length, 2 + count)
    };
    let total = head.checked_add(length)?;
    (total <= bytes.len()).then_some((total, head))
}

/// The SubjectPublicKeyInfo of an X.509 certificate.
pub fn public_key_info(certificate: &[u8]) -> Option<&[u8]> {
    let (_, head) = der_element(certificate)?;
    let inner = certificate.get(head..)?;
    let (tbs_total, tbs_head) = der_element(inner)?;
    let mut rest = inner.get(tbs_head..tbs_total)?;
    // The optional version is tagged [0].
    if *rest.first()? == 0xA0 {
        rest = rest.get(der_element(rest)?.0..)?;
    }
    // Serial, signature algorithm, issuer, validity, subject.
    for _ in 0..5 {
        rest = rest.get(der_element(rest)?.0..)?;
    }
    rest.get(..der_element(rest)?.0)
}

/// Checks a gateway's public key against its pin, for a self-signed gateway.
/// The handshake's signature is still verified, so the key must really be held.
#[derive(Debug)]
struct PinVerifier {
    pin: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let matches = public_key_info(end_entity).is_some_and(|key| Sha256::digest(key).as_slice() == self.pin);
        if matches {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("certificate does not match the pin".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn pinned_tls(pin: [u8; 32], identity: Option<&[u8]>) -> Result<rustls::ClientConfig, NetError> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::PrivateKeyDer;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Other(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier { pin, provider }));
    let mut config = match identity {
        Some(pem) => {
            let chain: Result<Vec<_>, _> = CertificateDer::pem_slice_iter(pem).collect();
            let chain = chain.map_err(|_| NetError::Other("the client certificate could not be read".into()))?;
            let key = PrivateKeyDer::from_pem_slice(pem)
                .map_err(|_| NetError::Other("the client certificate's key could not be read".into()))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|_| NetError::Other("the client certificate could not be used".into()))?
        }
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Same host, same port, and never from HTTPS down to HTTP.
fn redirect_allowed(from: &Url, to: &Url) -> bool {
    from.host_str() == to.host_str()
        && from.port_or_known_default() == to.port_or_known_default()
        && (to.scheme() == "https" || from.scheme() == to.scheme())
}

fn build_http(node: &WebNode) -> Result<reqwest::Client, NetError> {
    let policy = redirect::Policy::custom(|attempt| {
        let allowed = attempt
            .previous()
            .last()
            .is_some_and(|from| redirect_allowed(from, attempt.url()));
        if allowed && attempt.previous().len() < 5 {
            attempt.follow()
        } else {
            // Handed back as it is, for `refusal` to report.
            attempt.stop()
        }
    });
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STALL)
        .redirect(policy)
        .user_agent(concat!("ruthos/", env!("CARGO_PKG_VERSION")));
    if let Some(proxy) = node.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        let proxy = reqwest::Proxy::all(proxy).map_err(|_| NetError::Other("the proxy address is not valid".into()))?;
        builder = builder.proxy(proxy);
    }
    match (node.pin, &node.client_certificate) {
        (Some(pin), identity) => {
            builder = builder.use_preconfigured_tls(pinned_tls(pin, identity.as_deref())?);
        }
        (None, Some(pem)) => {
            let identity = reqwest::Identity::from_pem(pem)
                .map_err(|_| NetError::Other("the client certificate could not be read".into()))?;
            builder = builder.identity(identity);
        }
        (None, None) => {}
    }
    builder.build().map_err(|e| NetError::Other(e.to_string()))
}

/// Every cause of an error, for telling a certificate from anything else.
fn causes(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

pub(super) fn classify(e: reqwest::Error) -> NetError {
    let e = e.without_url();
    let text = causes(&e);
    let lower = text.to_lowercase();
    if lower.contains("certificate") || lower.contains("invalid peer") || lower.contains("handshake") {
        NetError::Certificate(text)
    } else if e.is_timeout() {
        NetError::Timeout
    } else if lower.contains("dns error") || lower.contains("no such host") || lower.contains("name or service") {
        NetError::Resolve(text)
    } else if lower.contains("refused") || lower.contains("10061") {
        NetError::Refused
    } else if e.is_connect() {
        NetError::Unreachable
    } else {
        NetError::Other(text)
    }
}

/// A DICOMweb node's client.
pub struct WebClient {
    pub(super) node: WebNode,
    pub(super) http: reqwest::Client,
    runtime: tokio::runtime::Runtime,
    /// Deviations already logged, as each is logged once per node.
    reported: parking_lot::Mutex<Vec<&'static str>>,
}

impl WebClient {
    pub fn new(node: WebNode) -> Result<Self, NetError> {
        let http = build_http(&node)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| NetError::Other(e.to_string()))?;
        Ok(Self {
            node,
            http,
            runtime,
            reported: Default::default(),
        })
    }

    pub(super) fn run<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    pub(super) fn report_deviation(&self, what: &'static str) {
        let mut reported = self.reported.lock();
        if !reported.contains(&what) {
            reported.push(what);
            tracing::warn!(node = %self.node.name, "DICOMweb server deviates from PS3.18: {what}");
        }
    }

    /// What a response means when it is not what was asked for.
    fn refusal(&self, response: &Response) -> Option<NetError> {
        let status = response.status();
        if status == StatusCode::FORBIDDEN {
            return Some(NetError::NotPermitted);
        }
        if status.is_redirection() {
            let target = response
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .unwrap_or("");
            let host = response
                .url()
                .join(target)
                .ok()
                .and_then(|u| u.host_str().map(String::from));
            let host = host.unwrap_or_default();
            // Access sends a person who is not signed in to its login page.
            if host == "cloudflareaccess.com" || host.ends_with(".cloudflareaccess.com") {
                return Some(NetError::SignInNeeded);
            }
            return Some(NetError::RedirectRefused(host));
        }
        None
    }

    /// Send a request made by `make`, signing it and handling refusals. A 401
    /// gets one silent renewal and one retry where the method allows.
    pub(super) async fn send(&self, make: impl Fn(&reqwest::Client) -> RequestBuilder) -> Result<Response, NetError> {
        let sign_in = &self.node.sign_in;
        sign_in.prepare().await?;
        let mut renewed = false;
        loop {
            let response = sign_in.apply(make(&self.http)).send().await.map_err(classify)?;
            if response.status() == StatusCode::UNAUTHORIZED {
                if !renewed && sign_in.renew().await? {
                    renewed = true;
                    continue;
                }
                tracing::warn!(node = %self.node.name, method = sign_in.method(), "sign-in refused (401)");
                return Err(NetError::SignInNeeded);
            }
            if let Some(refused) = self.refusal(&response) {
                tracing::warn!(node = %self.node.name, method = sign_in.method(), "refused: {refused}");
                return Err(refused);
            }
            return Ok(response);
        }
    }

    /// An HTML reply on a DICOMweb path: a login page or not a DICOMweb server.
    pub(super) fn not_dicom(&self, response: &Response, wanted: &str) -> NetError {
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|c| c.to_str().ok())
            .unwrap_or("nothing")
            .to_string();
        if self.node.sign_in.behind_access() && content_type.to_ascii_lowercase().starts_with("text/html") {
            return NetError::SignInNeeded;
        }
        NetError::NotDicomWeb(format!("expected {wanted}, got {content_type}"))
    }

    /// Read a whole reply, stopping at `most` bytes.
    pub(super) async fn read_limited(
        &self,
        mut response: Response,
        most: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, NetError> {
        let mut body = Vec::new();
        while let Some(chunk) = next_chunk(&mut response, cancel).await? {
            if body.len() + chunk.len() > most {
                return Err(NetError::TooLarge(format!(
                    "the reply is over {} MB",
                    most / (1024 * 1024)
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// The next piece of a reply, noticing a cancel even while waiting for one.
pub(super) async fn next_chunk(response: &mut Response, cancel: &AtomicBool) -> Result<Option<bytes::Bytes>, NetError> {
    let chunk = response.chunk();
    tokio::pin!(chunk);
    loop {
        tokio::select! {
            result = &mut chunk => return result.map_err(classify),
            () = tokio::time::sleep(CANCEL_POLL) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(NetError::Cancelled);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_follow_the_rules() {
        assert_eq!(
            check_address("https://gateway.example.org/dicom-web/", true).unwrap(),
            "https://gateway.example.org/dicom-web"
        );
        assert!(check_address("http://localhost:8042/dicom-web", false).is_ok());
        assert!(check_address("http://127.0.0.1:8042", false).is_ok());
        assert!(check_address("https://localhost:8042", false).is_ok());

        for (bad, signs_in) in [
            ("http://gateway.example.org", true),
            ("http://localhost.example.org", true),
            ("http://127.0.0.1.example.org", true),
            ("http://example.org#localhost", true),
            ("https://gateway.example.org", false),
            ("https://user:secret@gateway.example.org", true),
            ("ftp://gateway.example.org", true),
            ("gateway.example.org", true),
            ("", true),
        ] {
            assert!(check_address(bad, signs_in).is_err(), "{bad}");
        }
    }

    #[test]
    fn redirects_stay_on_the_same_host_and_never_go_down() {
        let url = |s: &str| Url::parse(s).unwrap();
        let from = url("https://a.example.org/x");
        assert!(redirect_allowed(&from, &url("https://a.example.org/y")));
        assert!(!redirect_allowed(&from, &url("http://a.example.org/y")));
        assert!(!redirect_allowed(&from, &url("https://b.example.org/y")));
        assert!(!redirect_allowed(&from, &url("https://a.example.org:8443/y")));
        assert!(redirect_allowed(
            &url("http://localhost:1/x"),
            &url("http://localhost:1/y")
        ));
    }

    #[test]
    fn pins_are_read_in_either_form() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let pin = parse_pin(hex).unwrap();
        assert_eq!(pin[1], 0x11);
        let colons: Vec<&str> = (0..32).map(|i| &hex[i * 2..i * 2 + 2]).collect();
        assert_eq!(parse_pin(&colons.join(":")), Some(pin));
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(pin);
        assert_eq!(parse_pin(&format!("sha256/{b64}")), Some(pin));
        assert_eq!(parse_pin("not a pin"), None);
    }

    #[test]
    fn broken_certificates_have_no_public_key() {
        assert_eq!(public_key_info(&[]), None);
        assert_eq!(public_key_info(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]), None);
        assert_eq!(public_key_info(&[0x30, 0x03, 0x30, 0x01, 0x00]), None);
    }
}
