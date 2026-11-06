//! Signing in to a DICOMweb node.
//!
//! Each node has its own method, because the gateways Ruthos meets differ. They
//! all sit behind one interface, which adds credentials to a request and says
//! whether a refusal can be recovered from, so adding a method touches nothing
//! else. Nothing here logs a credential, and `Debug` is not derived for anything
//! that holds one.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures_util::future::{BoxFuture, FutureExt};
use parking_lot::Mutex;
use reqwest::RequestBuilder;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::NetError;

/// Renew this long before a token expires.
const RENEW_EARLY: Duration = Duration::from_secs(60);
/// How long a person gets to finish signing in in their browser.
const BROWSER_WAIT: Duration = Duration::from_secs(300);
/// The most a token endpoint or discovery document may send.
const MOST_AUTH_BYTES: usize = 1024 * 1024;

/// How a node signs in.
pub trait SignIn: Send + Sync {
    /// The method's name, for logs.
    fn method(&self) -> &'static str;
    /// Make sure the credentials are fresh enough to send.
    fn prepare(&self) -> BoxFuture<'_, Result<(), NetError>> {
        async { Ok(()) }.boxed()
    }
    /// Add the credentials to a request.
    fn apply(&self, request: RequestBuilder) -> RequestBuilder;
    /// After a 401: renew silently if this method can. True to try again.
    fn renew(&self) -> BoxFuture<'_, Result<bool, NetError>> {
        async { Ok(false) }.boxed()
    }
    /// True behind Cloudflare Access, where HTML means signed out.
    fn behind_access(&self) -> bool {
        false
    }
}

/// No sign-in: localhost only.
pub struct NoSignIn;

impl SignIn for NoSignIn {
    fn method(&self) -> &'static str {
        "none"
    }
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request
    }
}

/// User name and password, for testing.
pub struct Basic {
    pub user: String,
    pub password: String,
}

impl SignIn for Basic {
    fn method(&self) -> &'static str {
        "basic"
    }
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request.basic_auth(&self.user, Some(&self.password))
    }
}

/// A Cloudflare Access service token, for unattended systems.
pub struct ServiceToken {
    pub client_id: String,
    pub secret: String,
}

impl SignIn for ServiceToken {
    fn method(&self) -> &'static str {
        "service token"
    }
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header("CF-Access-Client-Id", &self.client_id)
            .header("CF-Access-Client-Secret", &self.secret)
    }
    fn behind_access(&self) -> bool {
        true
    }
}

/// A Cloudflare Access application token. These have no refresh token, so an
/// expired one means signing in again.
pub struct AccessToken {
    pub token: String,
}

impl SignIn for AccessToken {
    fn method(&self) -> &'static str {
        "Cloudflare Access"
    }
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request.header("cf-access-token", &self.token)
    }
    fn behind_access(&self) -> bool {
        true
    }
}

/// Tokens from an OpenID Connect provider.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OidcTokens {
    pub access: String,
    pub refresh: Option<String>,
    /// Unix seconds.
    pub expires: u64,
    pub token_endpoint: String,
}

/// Told when tokens change, so they can go back to the credential store.
pub type TokensChanged = Arc<dyn Fn(&OidcTokens) + Send + Sync>;

/// OpenID Connect: a bearer token, renewed with its refresh token.
pub struct Oidc {
    tokens: Mutex<OidcTokens>,
    client_id: String,
    http: reqwest::Client,
    changed: TokensChanged,
}

impl Oidc {
    pub fn new(tokens: OidcTokens, client_id: &str, changed: TokensChanged) -> Result<Self, NetError> {
        Ok(Self {
            tokens: Mutex::new(tokens),
            client_id: client_id.to_string(),
            http: plain_client()?,
            changed,
        })
    }

    async fn refresh(&self) -> Result<bool, NetError> {
        let (refresh, endpoint) = {
            let t = self.tokens.lock();
            (t.refresh.clone(), t.token_endpoint.clone())
        };
        let Some(refresh) = refresh else { return Ok(false) };
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        let Ok(reply) = post_token(&self.http, &endpoint, &form).await else {
            // A revoked or expired refresh token: only a person can fix that.
            tracing::warn!("OpenID Connect renewal refused");
            return Ok(false);
        };
        let mut tokens = self.tokens.lock();
        tokens.access = reply.access_token;
        tokens.expires = expiry(reply.expires_in);
        if reply.refresh_token.is_some() {
            tokens.refresh = reply.refresh_token;
        }
        (self.changed)(&tokens);
        Ok(true)
    }
}

impl SignIn for Oidc {
    fn method(&self) -> &'static str {
        "OpenID Connect"
    }
    fn prepare(&self) -> BoxFuture<'_, Result<(), NetError>> {
        async move {
            let due = {
                let t = self.tokens.lock();
                t.refresh.is_some() && t.expires <= now() + RENEW_EARLY.as_secs()
            };
            if due {
                self.refresh().await?;
            }
            Ok(())
        }
        .boxed()
    }
    fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.tokens.lock().access)
    }
    fn renew(&self) -> BoxFuture<'_, Result<bool, NetError>> {
        self.refresh().boxed()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn expiry(expires_in: Option<u64>) -> u64 {
    now() + expires_in.unwrap_or(300)
}

/// A client for talking to an identity provider, which is not the gateway and
/// shares none of its pin or client certificate.
fn plain_client() -> Result<reqwest::Client, NetError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("ruthos/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| NetError::Other(e.to_string()))
}

#[derive(serde::Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(serde::Deserialize)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

async fn post_token(http: &reqwest::Client, endpoint: &str, form: &[(&str, &str)]) -> Result<TokenReply, NetError> {
    let reply = http
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|e| NetError::Other(format!("the sign-in server: {}", e.without_url())))?;
    let status = reply.status();
    let body = reply
        .bytes()
        .await
        .map_err(|e| NetError::Other(e.without_url().to_string()))?;
    if !status.is_success() || body.len() > MOST_AUTH_BYTES {
        tracing::warn!(status = status.as_u16(), "the token endpoint refused a request");
        return Err(NetError::SignInNeeded);
    }
    serde_json::from_slice(&body).map_err(|_| NetError::Other("the sign-in server's reply could not be read".into()))
}

fn random_text(bytes: usize) -> Result<String, NetError> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).map_err(|e| NetError::Other(e.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
}

/// The S256 challenge for a PKCE verifier (RFC 7636).
pub fn challenge_for(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// An issuer has to be HTTPS, except on this computer.
fn check_issuer(issuer: &str) -> Result<url::Url, NetError> {
    let url = url::Url::parse(issuer).map_err(|_| NetError::Insecure(format!("{issuer} is not an address")))?;
    if url.scheme() != "https" && !super::client::is_local(&url) {
        return Err(NetError::Insecure("an issuer must use HTTPS".into()));
    }
    Ok(url)
}

/// What to ask an OpenID Connect provider for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
}

/// Sign in through the system browser with PKCE and a loopback redirect, as
/// RFC 8252 describes, so that Ruthos never sees the password. `open` is given
/// the address to show the person.
pub async fn oidc_sign_in(settings: &OidcSettings, open: &(dyn Fn(&str) + Sync)) -> Result<OidcTokens, NetError> {
    let http = plain_client()?;
    let issuer = check_issuer(&settings.issuer)?;
    let well_known = format!(
        "{}/.well-known/openid-configuration",
        issuer.as_str().trim_end_matches('/')
    );
    let found: Discovery = http
        .get(&well_known)
        .send()
        .await
        .map_err(|e| NetError::Other(format!("the sign-in server: {}", e.without_url())))?
        .json()
        .await
        .map_err(|_| NetError::Other("that address is not an OpenID Connect issuer".into()))?;
    check_issuer(&found.authorization_endpoint)?;
    check_issuer(&found.token_endpoint)?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| NetError::Other(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| NetError::Other(e.to_string()))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let verifier = random_text(32)?;
    let state = random_text(16)?;
    let scopes = if settings.scopes.trim().is_empty() {
        "openid profile"
    } else {
        settings.scopes.trim()
    };
    let mut address = url::Url::parse(&found.authorization_endpoint).map_err(|e| NetError::Other(e.to_string()))?;
    address
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &settings.client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("scope", scopes)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge_for(&verifier))
        .append_pair("code_challenge_method", "S256");
    open(address.as_str());

    let code = tokio::time::timeout(BROWSER_WAIT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| NetError::Other("signing in took too long".into()))??;
    let reply = post_token(
        &http,
        &found.token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("client_id", settings.client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ],
    )
    .await?;
    Ok(OidcTokens {
        access: reply.access_token,
        refresh: reply.refresh_token,
        expires: expiry(reply.expires_in),
        token_endpoint: found.token_endpoint,
    })
}

/// Wait for the browser to come back with a code for this `state`. Anything
/// else that connects is told off and ignored, as any local program can try.
async fn wait_for_code(listener: &tokio::net::TcpListener, state: &str) -> Result<String, NetError> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|e| NetError::Other(e.to_string()))?;
        let mut head = vec![0u8; 8192];
        let n = stream.read(&mut head).await.unwrap_or(0);
        let text = String::from_utf8_lossy(&head[..n]).to_string();
        let target = text.split_whitespace().nth(1).unwrap_or("");
        let parsed = url::Url::parse(&format!("http://127.0.0.1{target}")).ok();
        let value = |name: &str| {
            parsed
                .as_ref()?
                .query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.to_string())
        };
        let right = value("state").as_deref() == Some(state);
        let page = if right {
            "You can close this window and go back to Ruthos."
        } else {
            "This was not the sign-in Ruthos was waiting for."
        };
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
        if !right {
            continue;
        }
        return match value("code") {
            Some(code) => Ok(code),
            None => Err(NetError::SignInNeeded),
        };
    }
}

/// Where Cloudflare's transfer service holds a finished sign-in.
pub const ACCESS_STORE: &str = "https://login.cloudflareaccess.org/";
/// Each poll waits up to a minute, so this is ten minutes in all.
const ACCESS_POLLS: usize = 10;

#[derive(serde::Deserialize)]
struct AccessReply {
    app_token: String,
}

/// The address that sends a person to the application's Access login, asking
/// for the result to go to the transfer service under `public_key`.
pub fn access_login_address(application: &str, audience: &str, public_key: &str) -> Result<String, NetError> {
    let mut address = url::Url::parse(application).map_err(|e| NetError::Insecure(e.to_string()))?;
    address
        .query_pairs_mut()
        .append_pair("token", public_key)
        .append_pair("aud", audience);
    let back = address.to_string();
    address
        .query_pairs_mut()
        .append_pair("redirect_url", &back)
        .append_pair("send_org_token", "true")
        .append_pair("edge_token_transfer", "true")
        .append_pair("close_interstitial", "true");
    address.set_path("cdn-cgi/access/cli");
    Ok(address.to_string())
}

/// Open what the transfer service sent: base64 of a nonce and a NaCl box, from
/// the service's key to ours. It never sees the token in the clear on our side
/// of the relay, and neither does anything else that handles the message.
pub fn open_access_reply(secret: &crypto_box::SecretKey, sealed: &[u8], sender: &str) -> Result<String, NetError> {
    use crypto_box::aead::Aead;
    let unreadable = || NetError::Other("the sign-in could not be read".to_string());
    let text = std::str::from_utf8(sealed).map_err(|_| unreadable())?.trim();
    let data = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| unreadable())?;
    let sender: [u8; 32] = base64::engine::general_purpose::URL_SAFE
        .decode(sender.trim())
        .map_err(|_| unreadable())?
        .try_into()
        .map_err(|_| unreadable())?;
    if data.len() < 24 {
        return Err(unreadable());
    }
    let (nonce, message) = data.split_at(24);
    let opened = crypto_box::SalsaBox::new(&crypto_box::PublicKey::from(sender), secret)
        .decrypt(nonce.into(), message)
        .map_err(|_| unreadable())?;
    let reply: AccessReply = serde_json::from_slice(&opened).map_err(|_| unreadable())?;
    Ok(reply.app_token)
}

/// Sign in to a Cloudflare Access application through the browser, in the way
/// `cloudflared access login` does. `store` is `ACCESS_STORE` outside tests.
pub async fn access_sign_in(
    application: &str,
    audience: &str,
    store: &str,
    open: &(dyn Fn(&str) + Sync),
) -> Result<String, NetError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| NetError::Other(e.to_string()))?;
    let secret = crypto_box::SecretKey::from(seed);
    let public_key = base64::engine::general_purpose::URL_SAFE.encode(secret.public_key().as_bytes());
    open(&access_login_address(application, audience, &public_key)?);

    let http = plain_client()?;
    let url = format!("{}/transfer/{public_key}", store.trim_end_matches('/'));
    for _ in 0..ACCESS_POLLS {
        let Ok(reply) = http.get(&url).send().await else {
            // A long poll that timed out: nobody has finished yet.
            continue;
        };
        let status = reply.status();
        if status.is_server_error() {
            return Err(NetError::Other(format!("the sign-in service answered {status}")));
        }
        if status != reqwest::StatusCode::OK {
            continue;
        }
        let sender = reply
            .headers()
            .get("service-public-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let sealed = reply
            .bytes()
            .await
            .map_err(|e| NetError::Other(e.without_url().to_string()))?;
        if sealed.len() > MOST_AUTH_BYTES {
            return Err(NetError::Other("the sign-in was too large to be one".into()));
        }
        if !sealed.is_empty() {
            return open_access_reply(&secret, &sealed, &sender);
        }
    }
    Err(NetError::Other("signing in took too long".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_login_address_has_what_cloudflared_sends() {
        let text = access_login_address("https://gateway.example.org/dicom-web", "aud1", "KEY").unwrap();
        let address = url::Url::parse(&text).unwrap();
        assert_eq!(address.path(), "/cdn-cgi/access/cli");
        let value = |key: &str| {
            address
                .query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string())
                .unwrap()
        };
        assert_eq!(value("token"), "KEY");
        assert_eq!(value("aud"), "aud1");
        assert_eq!(value("edge_token_transfer"), "true");
        assert_eq!(value("send_org_token"), "true");
        let back = url::Url::parse(&value("redirect_url")).unwrap();
        assert_eq!(back.path(), "/dicom-web", "back to the application itself");
        assert!(back.query().unwrap().contains("token=KEY"));
    }

    #[test]
    fn a_sealed_sign_in_opens_only_for_its_key() {
        use base64::engine::general_purpose::{STANDARD, URL_SAFE};
        use crypto_box::aead::Aead;
        let ours = crypto_box::SecretKey::from([1u8; 32]);
        let theirs = crypto_box::SecretKey::from([2u8; 32]);
        let nonce = [7u8; 24];
        let message = br#"{"app_token":"the.token","org_token":"x"}"#;
        let sealed = crypto_box::SalsaBox::new(&ours.public_key(), &theirs)
            .encrypt((&nonce).into(), &message[..])
            .unwrap();
        let wire = STANDARD.encode([&nonce[..], &sealed].concat());
        let sender = URL_SAFE.encode(theirs.public_key().as_bytes());
        assert_eq!(open_access_reply(&ours, wire.as_bytes(), &sender).unwrap(), "the.token");

        let stranger = crypto_box::SecretKey::from([3u8; 32]);
        assert!(open_access_reply(&stranger, wire.as_bytes(), &sender).is_err());
        assert!(open_access_reply(&ours, b"!!", &sender).is_err());
        assert!(open_access_reply(&ours, b"QUJD", &sender).is_err());
        assert!(open_access_reply(&ours, wire.as_bytes(), "short").is_err());
    }

    #[test]
    fn pkce_matches_the_rfc_example() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn an_issuer_must_be_secure_unless_it_is_this_computer() {
        assert!(check_issuer("https://login.example.org/realm").is_ok());
        assert!(check_issuer("http://localhost:8080/realm").is_ok());
        assert!(check_issuer("http://login.example.org").is_err());
        assert!(check_issuer("http://localhost.example.org").is_err());
    }
}
