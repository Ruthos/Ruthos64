//! The relay's HTTP calls. Every POST carries an idempotency key generated
//! for that user action, and a retry of the action sends the same key, so a
//! dropped connection never creates a second session or case.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::discovery::ServerInfo;
use crate::SessionError;

const RETRIES: usize = 3;

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    server: String,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// A session as `POST /v1/sessions` returns it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreatedSession {
    pub id: String,
    pub code: String,
    pub title: Option<String>,
    pub guests: bool,
    pub cases: Vec<String>,
    pub expires: Value,
}

/// A live session of this host's, for Rejoin.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSummary {
    pub id: String,
    pub code: String,
    pub title: Option<String>,
    pub state: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuestEntry {
    pub token: String,
    pub session: Value,
    pub expires: Value,
}

async fn decode<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, SessionError> {
    let status = r.status();
    if status.is_success() {
        return r
            .json()
            .await
            .map_err(|_| SessionError::Refused("The server sent an unreadable reply".to_string()));
    }
    let e: ApiError = r.json().await.unwrap_or(ApiError {
        code: String::new(),
        message: String::new(),
    });
    let code = if e.code.is_empty() {
        status.as_u16().to_string()
    } else {
        e.code
    };
    tracing::warn!(code = %code, "relay refused a request");
    Err(SessionError::Api {
        status: status.as_u16(),
        code,
        message: e.message,
    })
}

impl Http {
    /// `proxy`, when set, is used instead of `HTTPS_PROXY`.
    pub fn new(server: &str, proxy: Option<&str>) -> Result<Self, SessionError> {
        let mut b = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("ruthos/", env!("CARGO_PKG_VERSION")));
        if let Some(p) = proxy {
            let p =
                reqwest::Proxy::all(p).map_err(|_| SessionError::Refused("The proxy address is not valid".into()))?;
            b = b.proxy(p);
        }
        let client = b.build().map_err(|e| SessionError::Network(e.to_string()))?;
        Ok(Self {
            client,
            server: server.to_string(),
        })
    }

    pub async fn info(&self) -> Result<ServerInfo, SessionError> {
        let r = self
            .client
            .get(format!("{}/v1/info", self.server))
            .send()
            .await
            .map_err(network)?;
        decode(r).await
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        token: Option<&str>,
        path: &str,
        body: &Value,
        key: &str,
    ) -> Result<T, SessionError> {
        let url = format!("{}{path}", self.server);
        let mut last = None;
        for attempt in 0..RETRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
            }
            let mut q = self.client.post(&url).header("Idempotency-Key", key).json(body);
            if let Some(t) = token {
                q = q.bearer_auth(t);
            }
            match q.send().await {
                Ok(r) => return decode(r).await,
                Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => last = Some(e),
                Err(e) => return Err(network(e)),
            }
        }
        Err(last
            .map(network)
            .unwrap_or_else(|| SessionError::Network("no reply".into())))
    }

    pub async fn create_session(
        &self,
        token: &str,
        title: Option<&str>,
        cases: &[String],
        guests: bool,
        key: &str,
    ) -> Result<CreatedSession, SessionError> {
        let body = json!({"title": title, "cases": cases, "guests": guests});
        self.post(Some(token), "/v1/sessions", &body, key).await
    }

    pub async fn my_sessions(&self, token: &str) -> Result<Vec<SessionSummary>, SessionError> {
        #[derive(Deserialize)]
        struct List {
            sessions: Vec<SessionSummary>,
        }
        let r = self
            .client
            .get(format!("{}/v1/sessions", self.server))
            .bearer_auth(token)
            .send()
            .await
            .map_err(network)?;
        Ok(decode::<List>(r).await?.sessions)
    }

    pub async fn set_roster(&self, token: &str, sid: &str, emails: &[String]) -> Result<(), SessionError> {
        let r = self
            .client
            .put(format!("{}/v1/sessions/{sid}/roster", self.server))
            .bearer_auth(token)
            .json(&json!({"emails": emails}))
            .send()
            .await
            .map_err(network)?;
        decode::<Value>(r).await.map(|_| ())
    }

    pub async fn delete_session(&self, token: &str, sid: &str) -> Result<(), SessionError> {
        let r = self
            .client
            .delete(format!("{}/v1/sessions/{sid}", self.server))
            .bearer_auth(token)
            .send()
            .await
            .map_err(network)?;
        decode::<Value>(r).await.map(|_| ())
    }

    pub async fn guest(&self, code: &str, name: &str, key: &str) -> Result<GuestEntry, SessionError> {
        self.post(None, "/v1/guest", &json!({"code": code, "name": name}), key)
            .await
    }

    pub async fn post_json(&self, token: &str, path: &str, body: &Value, key: &str) -> Result<Value, SessionError> {
        self.post(Some(token), path, body, key).await
    }

    pub async fn get_json(&self, token: &str, path: &str) -> Result<Value, SessionError> {
        let r = self
            .client
            .get(format!("{}{path}", self.server))
            .bearer_auth(token)
            .send()
            .await
            .map_err(network)?;
        decode(r).await
    }

    /// Send bytes to a pre-signed address with exactly the given headers.
    pub async fn put_bytes(&self, url: &str, headers: &[(String, String)], bytes: Vec<u8>) -> Result<(), SessionError> {
        let mut q = self.client.put(url).timeout(Duration::from_secs(3600)).body(bytes);
        for (k, v) in headers {
            q = q.header(k.as_str(), v.as_str());
        }
        let r = q.send().await.map_err(network)?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(SessionError::Network(format!(
                "upload refused ({})",
                r.status().as_u16()
            )))
        }
    }

    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SessionError> {
        let r = self
            .client
            .get(url)
            .timeout(Duration::from_secs(3600))
            .send()
            .await
            .map_err(network)?;
        if !r.status().is_success() {
            return Err(SessionError::Network(format!(
                "download refused ({})",
                r.status().as_u16()
            )));
        }
        Ok(r.bytes().await.map_err(network)?.to_vec())
    }
}

fn network(e: reqwest::Error) -> SessionError {
    // The URL is left out: pre-signed addresses are credentials.
    SessionError::Network(e.without_url().to_string())
}

/// A key for one user action.
pub fn new_key() -> String {
    uuid::Uuid::new_v4().to_string()
}
