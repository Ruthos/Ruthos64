//! DICOMweb: searching, retrieving from and sending to a node over HTTPS
//! (the client side of DICOM PS3.18).
//!
//! Classic DICOM stays the route inside a practice network; this is the route
//! from anywhere else. Calls block like those in `scu`, and each client runs its
//! own small async runtime, so the windows treat both routes alike.

pub mod auth;
pub mod client;
pub mod multipart;
pub mod qido;
pub mod stow;
pub mod wado;

use std::sync::atomic::AtomicBool;

pub use auth::{
    access_sign_in, oidc_sign_in, AccessToken, Basic, NoSignIn, Oidc, OidcSettings, OidcTokens, ServiceToken, SignIn,
    ACCESS_STORE,
};
pub use client::{check_address, parse_pin, public_key_info, Transfer, WebClient, WebNode};
pub use qido::{Found, InstanceResult};

use crate::NetError;

/// Run a sign-in to the end, for a worker thread.
pub fn block_on<T>(future: impl std::future::Future<Output = Result<T, NetError>>) -> Result<T, NetError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| NetError::Other(e.to_string()))?
        .block_on(future)
}

/// One step of a connection test.
#[derive(Debug, Clone, PartialEq)]
pub struct TestStep {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

/// A connection test, step by step, stopping at the first failure so that a
/// certificate problem is not mistaken for a permission one. The last item is
/// whether `includefield=all` works, when that was found out.
pub fn test_connection(client: &WebClient) -> (Vec<TestStep>, Option<bool>) {
    let cancel = AtomicBool::new(false);
    let mut steps = Vec::new();
    let root = client.node.qido.clone();

    // 1. The address answers, over a connection that can be trusted.
    let reached = client.run(async { client.http.get(&root).send().await.map_err(client::classify) });
    let step = |name, result: Result<String, NetError>| TestStep {
        name,
        passed: result.is_ok(),
        detail: result.unwrap_or_else(|e| e.to_string()),
    };
    let first = step(
        "The address answers securely",
        reached.map(|r| format!("Answered with {}", r.status())),
    );
    let stop = !first.passed;
    steps.push(first);
    if stop {
        return (steps, None);
    }

    // 2. Sign-in is accepted. Only 401 and 403 fail here.
    let url = format!("{root}/studies");
    let query = [("limit".to_string(), "1".to_string())];
    let signed_in = client.run(client.page(&url, &query, &cancel));
    let refused = matches!(signed_in, Err(NetError::SignInNeeded | NetError::NotPermitted));
    let second = step(
        "Sign-in is accepted",
        match &signed_in {
            Err(e) if refused => Err(e.clone()),
            _ => Ok("Accepted".to_string()),
        },
    );
    steps.push(second);
    if refused {
        return (steps, None);
    }

    // 3. It speaks DICOMweb.
    let third = step(
        "A study search returns DICOM JSON",
        signed_in.map(|items| format!("{} study returned", items.len())),
    );
    let stop = !third.passed;
    steps.push(third);
    if stop {
        return (steps, None);
    }

    // 4. Whether the server takes includefield=all.
    let everything = [
        ("limit".to_string(), "1".to_string()),
        ("includefield".to_string(), "all".to_string()),
    ];
    let honoured = client.run(client.page(&url, &everything, &cancel)).is_ok();
    steps.push(TestStep {
        name: "includefield=all",
        passed: true,
        detail: if honoured {
            "Supported".to_string()
        } else {
            "Not supported, so Ruthos names the fields it needs".to_string()
        },
    });
    (steps, Some(honoured))
}
