//! Network failures, classified so they can be explained in plain words.
//!
//! Most PACS problems are one of a handful: the address is wrong, nothing
//! listens on the port, a firewall drops the connection, an AE title does
//! not match, or the PACS has not been told about Ruthos. `NetError` names
//! each, and `explain` turns it into what happened and what to do.

use std::io::ErrorKind;

use dicom_ul::pdu::{AssociationRJServiceUserReason, AssociationRJSource};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum NetError {
    #[error("cannot find the computer {0}")]
    Resolve(String),
    #[error("connection refused")]
    Refused,
    #[error("no answer (timed out)")]
    Timeout,
    #[error("network unreachable")]
    Unreachable,
    #[error("the PACS does not recognise its AE title")]
    CalledAeRejected,
    #[error("the PACS does not recognise Ruthos's AE title")]
    CallingAeRejected,
    #[error("association rejected: {0}")]
    Rejected(String),
    #[error("the PACS does not offer {0}")]
    NotSupported(&'static str),
    #[error("the PACS ended the association")]
    Aborted,
    #[error("the PACS does not know where to send the images")]
    MoveDestinationUnknown,
    #[error("the PACS reported status {0:04X}H")]
    Status(u16),
    #[error("cancelled")]
    Cancelled,
    #[error("sign-in is needed")]
    SignInNeeded,
    #[error("signed in, but not permitted to see this")]
    NotPermitted,
    #[error("the certificate could not be trusted: {0}")]
    Certificate(String),
    #[error("this is not a DICOMweb server: {0}")]
    NotDicomWeb(String),
    #[error("a redirect to {0} was refused")]
    RedirectRefused(String),
    #[error("stopped: {0}")]
    TooLarge(String),
    #[error("{0}")]
    Insecure(String),
    #[error("{0}")]
    Other(String),
}

impl NetError {
    fn from_io(e: &std::io::Error) -> NetError {
        match e.kind() {
            ErrorKind::ConnectionRefused => NetError::Refused,
            ErrorKind::TimedOut | ErrorKind::WouldBlock => NetError::Timeout,
            ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable | ErrorKind::AddrNotAvailable => {
                NetError::Unreachable
            }
            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => NetError::Aborted,
            _ => {
                let text = e.to_string();
                // Windows reports a connect timeout as os error 10060.
                if text.contains("10060") || text.to_lowercase().contains("timed out") {
                    NetError::Timeout
                } else {
                    NetError::Other(text)
                }
            }
        }
    }

    /// Classify an upper-layer error.
    pub fn from_ul(e: dicom_ul::association::Error) -> NetError {
        use dicom_ul::association::Error as E;
        match &e {
            E::ToAddress { source, .. } => NetError::Resolve(source.to_string()),
            E::Connect { source, .. } => {
                if source.kind() == ErrorKind::NotFound || source.to_string().contains("lookup") {
                    NetError::Resolve(source.to_string())
                } else {
                    NetError::from_io(source)
                }
            }
            E::Rejected { association_rj, .. } => match &association_rj.source {
                AssociationRJSource::ServiceUser(AssociationRJServiceUserReason::CalledAETitleNotRecognized) => {
                    NetError::CalledAeRejected
                }
                AssociationRJSource::ServiceUser(AssociationRJServiceUserReason::CallingAETitleNotRecognized) => {
                    NetError::CallingAeRejected
                }
                other => NetError::Rejected(other.to_string()),
            },
            E::NoAcceptedPresentationContexts { .. } => NetError::NotSupported("this service"),
            E::Aborted { .. } => NetError::Aborted,
            E::WireSend { source, .. } => NetError::from_io(source),
            _ => {
                // Receive failures carry the I/O error deep inside; the
                // text is enough to tell a timeout from a reset.
                let text = format!("{e}: {}", source_chain(&e));
                let lower = text.to_lowercase();
                if lower.contains("timed out") || lower.contains("10060") || lower.contains("would block") {
                    NetError::Timeout
                } else if lower.contains("reset") || lower.contains("aborted") || lower.contains("eof") {
                    NetError::Aborted
                } else {
                    NetError::Other(text)
                }
            }
        }
    }
}

fn source_chain(e: &dyn std::error::Error) -> String {
    let mut parts = Vec::new();
    let mut cur = e.source();
    while let Some(s) = cur {
        parts.push(s.to_string());
        cur = s.source();
    }
    parts.join(": ")
}

/// The PACS as configured in Ruthos.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub host: String,
    pub port: u16,
    pub ae_title: String,
}

/// This computer, as a PACS must be told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    pub ae_title: String,
    pub port: u16,
    pub address: String,
    pub listener_on: bool,
}

/// What went wrong and what to do about it, for people who are not
/// network engineers.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub summary: String,
    pub fix: String,
    pub technical: String,
}

pub fn explain(e: &NetError, node: &Node, me: &Me) -> Problem {
    let host = &node.host;
    let port = node.port;
    let tell_admin = format!(
        "Ask whoever runs the PACS to add Ruthos as a DICOM node: AE title {}, address {}, port {}.",
        me.ae_title, me.address, me.port
    );
    let (summary, fix) = match e {
        NetError::Resolve(_) => (
            format!("Ruthos can't find a computer called \"{host}\"."),
            "Check the spelling, or use the PACS's IP address instead (four numbers, like 192.168.1.20).".to_string(),
        ),
        NetError::Refused => (
            format!("{host} answered, but nothing is listening on port {port}."),
            "Check the port number (many PACS use 104, 11112 or 4242) and that the PACS software is running.".to_string(),
        ),
        NetError::Timeout => (
            format!("No answer from {host} on port {port}."),
            format!(
                "Check the address, that this computer is on the same network as the PACS (or that the VPN is connected), and that no firewall blocks port {port}."
            ),
        ),
        NetError::Unreachable => (
            format!("This computer has no route to {host}."),
            "Connect to the network the PACS is on (or the VPN), then try again.".to_string(),
        ),
        NetError::CalledAeRejected => (
            format!("The PACS doesn't answer to the AE title \"{}\".", node.ae_title),
            "AE titles must match exactly, capitals included, and are at most 16 characters. Check the PACS's own AE title in its settings.".to_string(),
        ),
        NetError::CallingAeRejected => ("The PACS doesn't know Ruthos yet.".to_string(), tell_admin),
        NetError::Rejected(reason) => (
            format!("The PACS refused the connection ({reason})."),
            format!("It may be busy, or only accept computers it has been told about. {tell_admin}"),
        ),
        NetError::NotSupported(what) => (
            format!("The PACS connected but doesn't offer {what} to Ruthos."),
            format!("It may not allow this from unknown computers. {tell_admin}"),
        ),
        NetError::Aborted => (
            "The PACS ended the conversation without saying why.".to_string(),
            format!("This is usually a permissions problem. {tell_admin}"),
        ),
        NetError::MoveDestinationUnknown => (
            "The PACS doesn't know where to send the images.".to_string(),
            format!(
                "Either set \"Retrieve with\" to Automatic or C-GET, or add Ruthos to the PACS: AE title {}, address {}, port {}.",
                me.ae_title, me.address, me.port
            ),
        ),
        NetError::Status(code) => (
            format!("The PACS reported a problem ({}).", status_words(*code)),
            "Try again; if it keeps happening, the PACS's own log will say more.".to_string(),
        ),
        NetError::Cancelled => ("Cancelled.".to_string(), String::new()),
        NetError::SignInNeeded => (
            format!("You need to sign in to {host}."),
            "Use Sign in beside this node under Preferences > PACS.".to_string(),
        ),
        NetError::NotPermitted => (
            "You're signed in, but not permitted to see this.".to_string(),
            "This isn't a connection problem. Ask whoever runs the server for access to these studies.".to_string(),
        ),
        NetError::Certificate(_) => (
            format!("Ruthos can't trust the certificate from {host}."),
            "If this server uses its own certificate, ask for its public key pin and add it to this node. Otherwise it may not be the server you think it is.".to_string(),
        ),
        NetError::NotDicomWeb(why) => (
            format!("{host} answered, but not as a DICOMweb server ({why})."),
            "Check the base address, which usually ends in /dicom-web.".to_string(),
        ),
        NetError::RedirectRefused(to) => (
            format!("{host} tried to send Ruthos to another address ({to}), and it was refused."),
            "Ruthos only follows redirects on the same server, so that a sign-in can't be sent elsewhere. Check the base address.".to_string(),
        ),
        NetError::TooLarge(what) => (
            format!("The server sent too much, so Ruthos stopped ({what})."),
            "Anything already received has been kept. Narrow the search, or retrieve one series at a time.".to_string(),
        ),
        NetError::Insecure(why) => (format!("This address can't be used: {why}."), String::new()),
        NetError::Other(text) => (format!("Something went wrong: {text}."), "Try again, and check ruthos.log for details.".to_string()),
    };
    Problem {
        summary,
        fix,
        technical: e.to_string(),
    }
}

/// A DIMSE status in words.
pub fn status_words(code: u16) -> String {
    let words = match code {
        0xA700..=0xA7FF => "out of resources",
        0xA801 => "move destination unknown",
        0xA900..=0xA9FF => "the request did not match what it supports",
        0xC000..=0xCFFF => "unable to process",
        0x0122 => "this kind of image is not supported",
        0x0124 => "not authorised",
        0xFE00 => "cancelled",
        _ => "unexpected status",
    };
    format!("{words}, status {code:04X}H")
}

/// This computer's address on the local network: the one used to reach
/// the outside world. No packet is sent.
pub fn local_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET-1, never routed anywhere real
    s.local_addr()
        .ok()
        .map(|a| a.ip().to_string())
        .filter(|ip| ip != "0.0.0.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> Node {
        Node {
            host: "pacs.local".into(),
            port: 104,
            ae_title: "PACS1".into(),
        }
    }

    fn me() -> Me {
        Me {
            ae_title: "RUTHOS".into(),
            port: 11112,
            address: "192.168.1.5".into(),
            listener_on: true,
        }
    }

    #[test]
    fn every_problem_names_a_fix() {
        for e in [
            NetError::Resolve("x".into()),
            NetError::Refused,
            NetError::Timeout,
            NetError::Unreachable,
            NetError::CalledAeRejected,
            NetError::CallingAeRejected,
            NetError::Rejected("no reason given".into()),
            NetError::NotSupported("queries"),
            NetError::Aborted,
            NetError::MoveDestinationUnknown,
            NetError::Status(0xA700),
        ] {
            let p = explain(&e, &node(), &me());
            assert!(!p.summary.is_empty() && !p.fix.is_empty(), "{e:?}");
        }
        let p = explain(&NetError::CallingAeRejected, &node(), &me());
        assert!(p.fix.contains("RUTHOS") && p.fix.contains("192.168.1.5") && p.fix.contains("11112"));
        assert!(explain(&NetError::Refused, &node(), &me()).summary.contains("port 104"));
    }

    #[test]
    fn io_errors_classify() {
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(NetError::from_io(&refused), NetError::Refused);
        let timeout = std::io::Error::other("A connection attempt failed (os error 10060)");
        assert_eq!(NetError::from_io(&timeout), NetError::Timeout);
    }

    #[test]
    fn connecting_to_a_closed_port_is_refused() {
        // Bind and drop a socket to find a port nothing listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let r = dicom_ul::association::client::ClientAssociationOptions::new()
            .with_abstract_syntax("1.2.840.10008.1.1")
            .calling_ae_title("RUTHOS")
            .called_ae_title("NOBODY")
            // Windows retries a refused connection for about two seconds
            // before reporting it, so the timeout must be longer than that.
            .connection_timeout(std::time::Duration::from_secs(15))
            .establish_with(&format!("127.0.0.1:{port}"));
        assert_eq!(r.err().map(NetError::from_ul), Some(NetError::Refused));
    }
}
