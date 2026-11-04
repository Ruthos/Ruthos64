//! `ruthos:` links. Exactly two forms are accepted, parsed strictly: custom
//! URL schemes have a long history of argument injection, so nothing else
//! gets through and no part of a link reaches a shell or a file path.

use crate::discovery::normalise;
use crate::SessionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Join { server: String, code: String },
    Profile { url: String },
}

fn refused() -> SessionError {
    SessionError::Refused("That is not a Ruthos link".to_string())
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    // Control characters have no place in an address or a code.
    (!text.chars().any(char::is_control)).then_some(text)
}

/// Exactly these keys, once each, in any order.
fn params<'a>(query: &'a str, keys: &[&str]) -> Result<Vec<(&'a str, String)>, SessionError> {
    let mut out: Vec<(&str, String)> = Vec::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').ok_or_else(refused)?;
        if !keys.contains(&k) || out.iter().any(|(seen, _)| *seen == k) {
            return Err(refused());
        }
        out.push((k, percent_decode(v).ok_or_else(refused)?));
    }
    if out.len() != keys.len() {
        return Err(refused());
    }
    Ok(out)
}

fn take(p: &[(&str, String)], key: &str) -> String {
    p.iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// A session's six-digit code.
pub fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse(link: &str) -> Result<Link, SessionError> {
    if link.len() > 2048 {
        return Err(refused());
    }
    if let Some(query) = link.strip_prefix("ruthos://join?") {
        let p = params(query, &["server", "code"])?;
        let code = take(&p, "code");
        if !valid_code(&code) {
            return Err(refused());
        }
        let server = normalise(&take(&p, "server"))?;
        return Ok(Link::Join { server, code });
    }
    if let Some(query) = link.strip_prefix("ruthos://profile?") {
        let p = params(query, &["url"])?;
        let url = take(&p, "url");
        let parsed = url::Url::parse(&url).map_err(|_| refused())?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(refused());
        }
        return Ok(Link::Profile { url });
    }
    Err(refused())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_forms_parse() {
        assert_eq!(
            parse("ruthos://join?server=https%3A%2F%2Frelay.example.org%2Fteach&code=482913").unwrap(),
            Link::Join {
                server: "https://relay.example.org/teach".into(),
                code: "482913".into()
            }
        );
        assert_eq!(
            parse("ruthos://join?code=482913&server=https://relay.example.org").unwrap(),
            Link::Join {
                server: "https://relay.example.org".into(),
                code: "482913".into()
            }
        );
        assert_eq!(
            parse("ruthos://profile?url=https://relay.example.org/a.ruthosprofile").unwrap(),
            Link::Profile {
                url: "https://relay.example.org/a.ruthosprofile".into()
            }
        );
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "ruthos://evil?server=https://a.example.org&code=482913",
            "ruthos://join/x?server=https://a.example.org&code=482913",
            "ruthos://join/?server=https://a.example.org&code=482913",
            "RUTHOS://join?server=https://a.example.org&code=482913",
            "ruthos:join?server=https://a.example.org&code=482913",
            "ruthos://join?server=https://a.example.org",
            "ruthos://join?server=https://a.example.org&code=482913&extra=1",
            "ruthos://join?server=https://a.example.org&code=482913&code=111111",
            "ruthos://join?server=http://a.example.org&code=482913",
            "ruthos://join?server=https://a.example.org&code=12345",
            "ruthos://join?server=https://a.example.org&code=12345a",
            "ruthos://join?server=https://a.example.org&code=482913%0d%0a",
            "ruthos://join?server=https://a.example.org%22%20--evil&code=482913",
            "ruthos://profile?url=file:///c:/windows/system32/calc.exe",
            "ruthos://profile?url=http://a.example.org/p",
            "ruthos://profile?url=https://a.example.org/p&x=1",
            "ruthos://",
            "https://a.example.org",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
