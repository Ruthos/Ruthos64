//! `.ruthosprofile` files. A profile can change only a short list of
//! low-risk settings, and it is rejected whole if anything in it falls
//! outside that list: partial application would let a malicious file hide
//! one forbidden change among many permitted ones. A signature proves who
//! issued the file and that nobody has altered it; it does not make the
//! issuer trustworthy, which is why the allowlist and the review dialog
//! carry the weight.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::discovery::{domain, normalise};
use crate::links::valid_code;
use crate::SessionError;

/// The largest profile Ruthos will read.
pub const MAX_PROFILE: usize = 1024 * 1024;

/// Allowed keys, by path. `*` is any key below that point.
const ALLOWED: &[&str] = &[
    "ruthos_profile",
    "issuer",
    "issuer.server",
    "issuer.name",
    "issuer.by",
    "issued",
    "expires",
    "server",
    "session",
    "session.code",
    "session.sid",
    "cases",
    "measurements",
    "measurements.library",
    "viewer",
    "viewer.echo_length_unit",
    "viewer.loupe",
    "viewer.loupe.magnification",
    "viewer.loupe.size",
    "viewer.loupe.enabled",
    "viewer.dial",
    "viewer.dial.modes",
    "viewer.default_layout",
    "sig",
    "sig.alg",
    "sig.kid",
    "sig.value",
];

/// Sections the user can tick in the review dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Section {
    Server,
    Session,
    Cases,
    Measurements,
    Viewer,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Server => "Session server",
            Section::Session => "Session to join",
            Section::Cases => "Teaching cases to download",
            Section::Measurements => "Measurements library",
            Section::Viewer => "Viewer preferences",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verification {
    /// Signed by the server's key.
    Verified,
    Unsigned,
}

/// A signing key that differs from the one first seen for a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChange {
    pub pinned: String,
    pub presented: String,
}

/// A profile that passed every check, ready for the review dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub value: Value,
    pub verification: Verification,
    pub issuer_name: String,
    pub issuer_by: String,
    /// The server named by the issuer, normalised.
    pub issuer_server: Option<String>,
    pub expires: Option<DateTime<Utc>>,
    pub sections: Vec<Section>,
    pub key_change: Option<KeyChange>,
    /// Key id and key to pin for the issuing server on Apply.
    pub pin: Option<(String, String)>,
}

fn reject(why: &str) -> SessionError {
    SessionError::Refused(format!("This profile was not applied: {why}"))
}

/// The bytes a profile's signature covers: the object without `sig`, keys
/// sorted, no whitespace, non-ASCII kept as is. The same as Python's
/// `json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`
/// for the strings, integers and booleans a profile holds.
pub fn canonical(value: &Value) -> String {
    fn write(out: &mut String, v: &Value) {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<&String, &Value> = m.iter().collect();
                out.push('{');
                for (i, (k, v)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    write(out, v);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(out, v);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(&mut out, value);
    out
}

fn check_keys(path: &str, map: &Map<String, Value>) -> Result<(), SessionError> {
    for (k, v) in map {
        let full = if path.is_empty() {
            k.clone()
        } else {
            format!("{path}.{k}")
        };
        // The library's contents are the measurements editor's business,
        // and its import checks them; only credentials are looked for here.
        if full == "measurements.library" {
            continue;
        }
        if !ALLOWED.contains(&full.as_str()) {
            return Err(reject(&format!(
                "it tries to set \"{full}\", which a profile may not change"
            )));
        }
        if let Value::Object(inner) = v {
            check_keys(&full, inner)?;
        }
    }
    Ok(())
}

fn credential_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "token",
        "secret",
        "credential",
        "apikey",
        "api_key",
        "bearer",
    ]
    .iter()
    .any(|w| k.contains(w))
}

fn credential_value(s: &str) -> bool {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return true;
    }
    // A signed token: base64url parts joined by dots, the first one JSON.
    t.starts_with("eyJ") && t.contains('.') && !t.contains(char::is_whitespace)
}

/// Anywhere in the file, library included, a credential is a refusal.
fn holds_credential(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| credential_key(k) || holds_credential(v)),
        Value::Array(a) => a.iter().any(holds_credential),
        Value::String(s) => credential_value(s),
        _ => false,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedKey {
    pub kid: String,
    pub key: String,
}

/// Keys first seen for each server.
pub type Pins = BTreeMap<String, PinnedKey>;

/// Everything but the review: size, allowlist, credentials, expiry and
/// signature. `keys` are the signing keys published by the profile's server.
pub fn check(
    text: &str,
    now: DateTime<Utc>,
    keys: &BTreeMap<String, String>,
    pins: &Pins,
) -> Result<Checked, SessionError> {
    if text.len() > MAX_PROFILE {
        return Err(reject("it is too large"));
    }
    let value: Value = serde_json::from_str(text).map_err(|_| reject("it is not a Ruthos profile"))?;
    let map = value.as_object().ok_or_else(|| reject("it is not a Ruthos profile"))?;
    if map.get("ruthos_profile").and_then(Value::as_u64) != Some(1) {
        return Err(reject("it is not a version Ruthos understands"));
    }
    check_keys("", map)?;
    if holds_credential(&value) {
        return Err(reject("it contains a credential"));
    }

    let text_of = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
    let expires = match map.get("expires") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .ok_or_else(|| reject("its expiry date cannot be read"))?
                .with_timezone(&Utc),
        ),
    };
    if expires.is_some_and(|e| e < now) {
        return Err(reject("it has expired"));
    }

    let issuer = map.get("issuer").and_then(Value::as_object);
    let issuer_server = match issuer.and_then(|i| i.get("server")).and_then(Value::as_str) {
        Some(s) => Some(normalise(s).map_err(|_| reject("its issuer is not a valid server"))?),
        None => None,
    };
    let server = match map.get("server") {
        Some(s) => {
            Some(normalise(s.as_str().unwrap_or_default()).map_err(|_| reject("its server address is not valid"))?)
        }
        None => None,
    };
    if let Some(code) = map.get("session").and_then(|s| s.get("code")) {
        if !code.as_str().is_some_and(valid_code) {
            return Err(reject("its session code is not valid"));
        }
    }
    if let Some(cases) = map.get("cases") {
        let ok = cases
            .as_array()
            .is_some_and(|a| a.iter().all(|c| c.as_str().is_some_and(crate::cases::valid_reference)));
        if !ok {
            return Err(reject("its teaching case list is not valid"));
        }
    }

    let mut sections = Vec::new();
    for (key, section) in [
        ("server", Section::Server),
        ("session", Section::Session),
        ("cases", Section::Cases),
        ("measurements", Section::Measurements),
        ("viewer", Section::Viewer),
    ] {
        if map.contains_key(key) {
            sections.push(section);
        }
    }

    // The signature.
    let mut key_change = None;
    let mut pin = None;
    let verification = match map.get("sig") {
        None => Verification::Unsigned,
        Some(sig) => {
            let kid = text_of(sig.get("kid"));
            if sig.get("alg").and_then(Value::as_str) != Some("Ed25519") {
                return Err(reject("its signature uses an unknown method"));
            }
            let encoded = keys
                .get(&kid)
                .ok_or_else(|| reject("its signature uses an unknown key"))?;
            let mut body = map.clone();
            body.remove("sig");
            verify(&canonical(&Value::Object(body)), &text_of(sig.get("value")), encoded)
                .map_err(|_| reject("its signature does not verify"))?;
            if let Some(server) = &issuer_server {
                match pins.get(server) {
                    Some(p) if p.key != *encoded => {
                        key_change = Some(KeyChange {
                            pinned: p.kid.clone(),
                            presented: kid.clone(),
                        })
                    }
                    Some(_) => {}
                    None => pin = Some((kid.clone(), encoded.clone())),
                }
            }
            Verification::Verified
        }
    };

    if verification == Verification::Unsigned
        && sections
            .iter()
            .any(|s| !matches!(s, Section::Measurements | Section::Viewer))
    {
        return Err(reject(
            "an unsigned profile may carry only a measurements library and viewer preferences",
        ));
    }
    // A profile that names a server must be signed by that same server.
    if let Some(server) = &server {
        if issuer_server.as_ref() != Some(server) {
            return Err(reject("it names a server other than the one that signed it"));
        }
    }

    Ok(Checked {
        verification,
        issuer_name: text_of(issuer.and_then(|i| i.get("name"))),
        issuer_by: text_of(issuer.and_then(|i| i.get("by"))),
        issuer_server,
        expires,
        sections,
        key_change,
        pin,
        value,
    })
}

fn verify(message: &str, signature: &str, key: &str) -> Result<(), ()> {
    let key: [u8; 32] = STANDARD.decode(key).map_err(|_| ())?.try_into().map_err(|_| ())?;
    let sig: [u8; 64] = STANDARD.decode(signature).map_err(|_| ())?.try_into().map_err(|_| ())?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| ())?;
    key.verify_strict(message.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| ())
}

impl Checked {
    /// The first line of the review dialog: who, and where.
    pub fn headline(&self) -> String {
        match &self.issuer_server {
            Some(s) => domain(s),
            None if self.issuer_name.is_empty() => "Unknown issuer".to_string(),
            None => self.issuer_name.clone(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-04T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const SERVER: &str = "https://relay.example.org/teach";

    fn signed(mut p: Value) -> (String, BTreeMap<String, String>) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let sig = key.sign(canonical(&p).as_bytes());
        p["sig"] = json!({"alg":"Ed25519","kid":"2026-11","value":STANDARD.encode(sig.to_bytes())});
        let keys = BTreeMap::from([("2026-11".to_string(), STANDARD.encode(key.verifying_key().to_bytes()))]);
        (p.to_string(), keys)
    }

    fn full() -> Value {
        json!({
            "ruthos_profile": 1,
            "issuer": {"server": SERVER, "name": "Example Teaching", "by": "Dr A Educator"},
            "issued": "2026-11-04T09:00:00Z",
            "expires": "2027-07-31T23:59:59Z",
            "server": SERVER,
            "session": {"code": "482913"},
            "cases": ["EQ-CARD-2026-014"],
            "measurements": {"library": {"definitions": []}},
            "viewer": {"echo_length_unit": "cm", "loupe": {"magnification": 4}}
        })
    }

    /// A signature made independently, with Python's cryptography library,
    /// under the key [7; 32]: a change of Ed25519 crate must still verify
    /// profiles that were signed before it.
    #[test]
    fn verifies_an_independently_made_signature() {
        let key = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";
        let sig = "kElKzYgBiBjwMwFisSOV96dF62bUVYMH66dg4iWsH2+1iI0FZU0R30j2Rb5D2dwp27bwFDI++631MN6MfmnmBQ==";
        let message = r#"{"issuer":{"name":"Example Teaching"},"ruthos_profile":1}"#;
        assert_eq!(
            STANDARD.encode(SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes()),
            key,
            "the test key is the one the vector was made with"
        );
        assert!(verify(message, sig, key).is_ok());
        assert!(verify(&message.replace('1', "2"), sig, key).is_err());
    }

    #[test]
    fn canonical_form_sorts_keys_without_whitespace() {
        assert_eq!(
            canonical(&json!({"b":[1,{"z":"é","a":true}],"a":null})),
            r#"{"a":null,"b":[1,{"a":true,"z":"é"}]}"#
        );
    }

    #[test]
    fn a_signed_profile_passes_and_lists_its_sections() {
        let (text, keys) = signed(full());
        let c = check(&text, now(), &keys, &Pins::new()).unwrap();
        assert_eq!(c.verification, Verification::Verified);
        assert_eq!(c.sections.len(), 5);
        assert_eq!(c.headline(), "relay.example.org");
        assert!(c.pin.is_some());
    }

    #[test]
    fn one_forbidden_key_rejects_the_whole_file() {
        for (path, val) in [
            ("listener_port", json!(104)),
            ("pacs_nodes", json!([{"host": "a"}])),
            ("data_root", json!("c:/x")),
            ("watch_folders", json!([])),
            ("proxy", json!("http://x")),
        ] {
            let mut p = full();
            p[path] = val;
            let (text, keys) = signed(p);
            let e = check(&text, now(), &keys, &Pins::new()).unwrap_err().to_string();
            assert!(e.contains(path), "{e}");
        }
        let mut p = full();
        p["viewer"]["listener"] = json!(1);
        let (text, keys) = signed(p);
        assert!(check(&text, now(), &keys, &Pins::new()).is_err());
    }

    #[test]
    fn a_credential_is_refused_even_inside_the_library() {
        let mut p = full();
        p["measurements"]["library"] = json!({"note": "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOjF9.abc"});
        let (text, keys) = signed(p);
        assert!(check(&text, now(), &keys, &Pins::new())
            .unwrap_err()
            .to_string()
            .contains("credential"));
        let mut p = full();
        p["measurements"]["library"] = json!({"api_token": "x"});
        let (text, keys) = signed(p);
        assert!(check(&text, now(), &keys, &Pins::new()).is_err());
    }

    #[test]
    fn a_failed_signature_or_past_expiry_is_refused() {
        let (text, keys) = signed(full());
        let tampered = text.replace("Dr A Educator", "Dr B Other");
        assert!(check(&tampered, now(), &keys, &Pins::new()).is_err());
        let mut p = full();
        p["expires"] = json!("2026-01-01T00:00:00Z");
        let (text, keys) = signed(p);
        assert!(check(&text, now(), &keys, &Pins::new())
            .unwrap_err()
            .to_string()
            .contains("expired"));
    }

    #[test]
    fn unsigned_profiles_carry_only_low_risk_sections() {
        let unsigned = json!({"ruthos_profile":1,"measurements":{"library":{}},"viewer":{"echo_length_unit":"mm"}});
        let c = check(&unsigned.to_string(), now(), &BTreeMap::new(), &Pins::new()).unwrap();
        assert_eq!(c.verification, Verification::Unsigned);
        let with_server = json!({"ruthos_profile":1,"server":SERVER});
        assert!(check(&with_server.to_string(), now(), &BTreeMap::new(), &Pins::new()).is_err());
    }

    #[test]
    fn a_server_must_sign_for_itself() {
        let mut p = full();
        p["server"] = json!("https://elsewhere.example.org");
        let (text, keys) = signed(p);
        assert!(check(&text, now(), &keys, &Pins::new()).is_err());
    }

    #[test]
    fn a_changed_key_is_shown_naming_both() {
        let (text, keys) = signed(full());
        let pins = Pins::from([(
            SERVER.to_string(),
            PinnedKey {
                kid: "2025-01".into(),
                key: STANDARD.encode([9u8; 32]),
            },
        )]);
        let c = check(&text, now(), &keys, &pins).unwrap();
        assert_eq!(
            c.key_change,
            Some(KeyChange {
                pinned: "2025-01".into(),
                presented: "2026-11".into()
            })
        );
    }
}
