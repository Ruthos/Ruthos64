//! Deterministic, irreversible UID derivation.
//!
//! A teaching copy keeps its internal structure (study, series, instances,
//! frame of reference, references between instances) but must not carry a
//! UID a source PACS could match. Each original UID maps to
//! `2.25.<first 128 bits of HMAC-SHA256(secret, original) as decimal>`, the
//! UUID-derived form of PS3.5 Annex B.2, which is at most 44 characters.
//!
//! The secret is per case: within a case the mapping is stable, so a
//! corrected re-export replaces the earlier copy; across cases the same
//! source yields unrelated UIDs; and without the secret the mapping can be
//! neither reversed nor tested.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Derive the teaching-copy UID for `original` under a case secret.
pub fn derive_uid(secret: &[u8; 32], original: &str) -> String {
    let original = original.trim_end_matches('\0').trim();
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(original.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut first = [0u8; 16];
    first.copy_from_slice(&digest[..16]);
    format!("2.25.{}", u128::from_be_bytes(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_within_a_secret_and_distinct_across_secrets() {
        let a = [7u8; 32];
        let b = [9u8; 32];
        let u1 = derive_uid(&a, "1.2.840.113619.2.1");
        let u2 = derive_uid(&a, "1.2.840.113619.2.1");
        let u3 = derive_uid(&b, "1.2.840.113619.2.1");
        let u4 = derive_uid(&a, "1.2.840.113619.2.2");
        assert_eq!(u1, u2);
        assert_ne!(u1, u3);
        assert_ne!(u1, u4);
        assert!(u1.starts_with("2.25."));
        assert!(u1.len() <= 64, "{u1}");
        assert!(u1[5..].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn ignores_trailing_padding() {
        let s = [1u8; 32];
        assert_eq!(derive_uid(&s, "1.2.3\0"), derive_uid(&s, "1.2.3"));
    }
}
