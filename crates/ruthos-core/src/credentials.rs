//! Secrets for DICOMweb nodes, in the operating system's credential store:
//! Windows Credential Manager, the macOS Keychain or the Secret Service.
//!
//! Nothing here reaches settings, logs or crash reports. Windows holds at most
//! 2,560 bytes of UTF-16 in one credential, and a signed token or a client
//! certificate is often longer, so a value is split across entries. The first
//! says how many there are.

const SERVICE: &str = "Ruthos DICOMweb";
/// Half of Windows's limit, in characters, with room to spare.
const MOST_CHARS: usize = 1000;
/// More than any real secret needs.
const MOST_PIECES: usize = 64;

/// What is being kept for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secret {
    /// A Basic password or a service token's secret.
    Password,
    /// A signed-in session: OpenID Connect tokens or an Access token.
    Session,
    /// A client certificate and its key, as PEM.
    Certificate,
}

impl Secret {
    const ALL: [Secret; 3] = [Secret::Password, Secret::Session, Secret::Certificate];

    fn name(self) -> &'static str {
        match self {
            Secret::Password => "password",
            Secret::Session => "session",
            Secret::Certificate => "certificate",
        }
    }
}

fn entry(node: &str, what: Secret, piece: usize) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("{node}/{}/{piece}", what.name())).map_err(|e| e.to_string())
}

fn pieces(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    chars.chunks(MOST_CHARS).map(|c| c.iter().collect()).collect()
}

/// Keep a secret for a node, replacing any before it.
pub fn store(node: &str, what: Secret, value: &str) -> Result<(), String> {
    let parts = pieces(value);
    if parts.len() > MOST_PIECES {
        return Err("that is too long to keep".to_string());
    }
    delete(node, what);
    for (i, part) in parts.iter().enumerate() {
        entry(node, what, i + 1)?
            .set_password(part)
            .map_err(|e| e.to_string())?;
    }
    entry(node, what, 0)?
        .set_password(&parts.len().to_string())
        .map_err(|e| e.to_string())
}

/// A node's secret, if it has one.
pub fn load(node: &str, what: Secret) -> Option<String> {
    let count: usize = entry(node, what, 0).ok()?.get_password().ok()?.parse().ok()?;
    if count > MOST_PIECES {
        return None;
    }
    let mut value = String::new();
    for i in 1..=count {
        value.push_str(&entry(node, what, i).ok()?.get_password().ok()?);
    }
    Some(value)
}

/// Forget one secret.
pub fn delete(node: &str, what: Secret) {
    let count: usize = entry(node, what, 0)
        .ok()
        .and_then(|e| e.get_password().ok())
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    for i in 0..=count.min(MOST_PIECES) {
        if let Ok(e) = entry(node, what, i) {
            let _ = e.delete_credential();
        }
    }
}

/// Forget everything held for a node, when it is deleted.
pub fn delete_node(node: &str) {
    for what in Secret::ALL {
        delete(node, what);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_split_and_join() {
        let value = "é".repeat(MOST_CHARS * 2 + 5);
        let parts = pieces(&value);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts.concat(), value);
        assert!(parts.iter().all(|p| p.encode_utf16().count() * 2 <= 2560));
        assert!(pieces("").is_empty());
    }

    // Against the real store, which a build machine without one does not have.
    #[cfg(windows)]
    #[test]
    fn a_token_longer_than_one_credential_survives_the_store() {
        let node = format!("test-{}", uuid::Uuid::new_v4().simple());
        let value = "t".repeat(4000);
        store(&node, Secret::Session, &value).unwrap();
        assert_eq!(load(&node, Secret::Session).as_deref(), Some(value.as_str()));
        store(&node, Secret::Session, "short").unwrap();
        assert_eq!(load(&node, Secret::Session).as_deref(), Some("short"));
        delete_node(&node);
        assert_eq!(load(&node, Secret::Session), None);
    }
}
