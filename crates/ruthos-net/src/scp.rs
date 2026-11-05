//! C-STORE service class provider: the DICOM listener.
//!
//! Adapted from the dicom-rs `storescp` tool. Each accepted association runs
//! on its own thread; received instances are written to the configured
//! folder as `<SOP Instance UID>.dcm` (through a `.part` temporary so a
//! folder watcher never sees a half-written file).

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_encoding::transfer_syntax::TransferSyntaxIndex;
use dicom_object::{FileMetaTableBuilder, InMemDicomObject, StandardDataDictionary};
use dicom_transfer_syntax_registry::TransferSyntaxRegistry;
use dicom_ul::association::server::ServerAssociationOptions;
use dicom_ul::association::Association;
use dicom_ul::pdu::{PDataValue, PDataValueType};
use dicom_ul::Pdu;

#[derive(Debug, Clone)]
pub struct ListenerConfig {
    pub port: u16,
    pub ae_title: String,
    pub out_dir: PathBuf,
    /// The devices allowed to send, shared so Preferences can change it
    /// while the listener runs. Empty: nothing is accepted.
    pub allow: Arc<std::sync::Mutex<Vec<AllowEntry>>>,
    /// Accept SOP classes Ruthos does not recognise (an advanced option,
    /// off by default).
    pub promiscuous: bool,
}

/// A device allowed to send to the listener: its calling AE title and,
/// optionally, the address or subnet it must connect from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    pub name: String,
    pub ae_title: String,
    /// `192.168.1.20`, `192.168.1.0/24`, `fe80::1` or empty for any.
    pub address: String,
}

/// Whether `ip` matches an address or subnet written as `a.b.c.d`,
/// `a.b.c.d/n` or the IPv6 equivalents. An empty spec matches anything; a
/// spec that does not parse matches nothing.
pub fn address_matches(spec: &str, ip: Option<std::net::IpAddr>) -> bool {
    use std::net::IpAddr;
    let spec = spec.trim();
    if spec.is_empty() {
        return true;
    }
    let Some(ip) = ip else { return false };
    // An IPv4 peer seen through an IPv6 socket.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    let (addr, bits) = match spec.split_once('/') {
        Some((a, n)) => match n.trim().parse::<u32>() {
            Ok(n) => (a.trim(), Some(n)),
            Err(_) => return false,
        },
        None => (spec, None),
    };
    let Ok(net) = addr.parse::<IpAddr>() else { return false };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            let bits = bits.unwrap_or(32).min(32);
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            (u32::from(n) & mask) == (u32::from(i) & mask)
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => {
            let bits = bits.unwrap_or(128).min(128);
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            (u128::from(n) & mask) == (u128::from(i) & mask)
        }
        _ => false,
    }
}

/// The access check: a listed calling AE title from its listed address.
struct Allowlist {
    entries: Vec<AllowEntry>,
    peer: Option<std::net::IpAddr>,
    /// The calling AE title of a refused request, for the log.
    refused: Arc<std::sync::Mutex<Option<String>>>,
}

impl dicom_ul::association::server::AccessControl for Allowlist {
    fn check_access(
        &self,
        _this_ae_title: &str,
        calling_ae_title: &str,
        _called_ae_title: &str,
        _user_identity: Option<&dicom_ul::pdu::UserIdentity>,
    ) -> Result<(), dicom_ul::pdu::AssociationRJServiceUserReason> {
        let calling = calling_ae_title.trim();
        let ok = self
            .entries
            .iter()
            .any(|e| e.ae_title.trim().eq_ignore_ascii_case(calling) && address_matches(&e.address, self.peer));
        if ok {
            Ok(())
        } else {
            if let Ok(mut r) = self.refused.lock() {
                *r = Some(calling.to_string());
            }
            Err(dicom_ul::pdu::AssociationRJServiceUserReason::CallingAETitleNotRecognized)
        }
    }
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    Started {
        port: u16,
        ae_title: String,
    },
    Association {
        peer_ae: String,
        peer_addr: String,
    },
    /// A device not on the allow list tried to connect.
    Refused {
        peer_ae: String,
        peer_addr: String,
    },
    Stored {
        path: PathBuf,
        sop_instance_uid: String,
        peer_ae: String,
    },
    Echo {
        peer_ae: String,
    },
    Released {
        peer_ae: String,
        stored: usize,
    },
    Error(String),
    Stopped,
}

pub struct StoreScp {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub events: Receiver<NetEvent>,
    pub config: ListenerConfig,
}

/// Storage SOP classes announced explicitly. Promiscuous mode accepts
/// anything else a modality proposes, which matters for veterinary vendors.
#[allow(deprecated)]
pub(crate) const STORAGE_ABSTRACT_SYNTAXES: &[&str] = &[
    uids::COMPUTED_RADIOGRAPHY_IMAGE_STORAGE,
    uids::DIGITAL_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    uids::DIGITAL_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    uids::DIGITAL_INTRA_ORAL_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    uids::DIGITAL_INTRA_ORAL_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    uids::CT_IMAGE_STORAGE,
    uids::ENHANCED_CT_IMAGE_STORAGE,
    uids::MR_IMAGE_STORAGE,
    uids::ENHANCED_MR_IMAGE_STORAGE,
    uids::ULTRASOUND_IMAGE_STORAGE,
    uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE,
    uids::ULTRASOUND_IMAGE_STORAGE_RETIRED,
    uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE_RETIRED,
    uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    uids::MULTI_FRAME_GRAYSCALE_BYTE_SECONDARY_CAPTURE_IMAGE_STORAGE,
    uids::MULTI_FRAME_GRAYSCALE_WORD_SECONDARY_CAPTURE_IMAGE_STORAGE,
    uids::MULTI_FRAME_TRUE_COLOR_SECONDARY_CAPTURE_IMAGE_STORAGE,
    uids::X_RAY_ANGIOGRAPHIC_IMAGE_STORAGE,
    uids::X_RAY_RADIOFLUOROSCOPIC_IMAGE_STORAGE,
    uids::NUCLEAR_MEDICINE_IMAGE_STORAGE,
    uids::POSITRON_EMISSION_TOMOGRAPHY_IMAGE_STORAGE,
    uids::SEGMENTATION_STORAGE,
    uids::ENCAPSULATED_PDF_STORAGE,
    uids::GRAYSCALE_SOFTCOPY_PRESENTATION_STATE_STORAGE,
    uids::BASIC_TEXT_SR_STORAGE,
    uids::ENHANCED_SR_STORAGE,
    uids::VERIFICATION,
];

impl StoreScp {
    /// Bind the port and start accepting associations.
    pub fn start(config: ListenerConfig) -> std::io::Result<Self> {
        std::fs::create_dir_all(&config.out_dir)?;
        let listener = TcpListener::bind(("0.0.0.0", config.port))?;
        listener.set_nonblocking(true)?;
        let (tx, events) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let cfg = config.clone();
        let handle = std::thread::Builder::new()
            .name("ruthos-storescp".into())
            .spawn(move || accept_loop(listener, cfg, tx, stop2))?;
        Ok(Self {
            stop,
            handle: Some(handle),
            events,
            config,
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StoreScp {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: TcpListener, config: ListenerConfig, tx: Sender<NetEvent>, stop: Arc<AtomicBool>) {
    let _ = tx.send(NetEvent::Started {
        port: config.port,
        ae_title: config.ae_title.clone(),
    });
    tracing::info!("DICOM listener {} on port {}", config.ae_title, config.port);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, addr)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
                let cfg = config.clone();
                let tx = tx.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("ruthos-scp-{addr}"))
                    .spawn(move || {
                        if let Err(e) = handle_association(stream, &cfg, &tx) {
                            tracing::warn!("association from {addr} ended with error: {e}");
                            let _ = tx.send(NetEvent::Error(format!("{addr}: {e}")));
                        }
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = tx.send(NetEvent::Error(format!("accept failed: {e}")));
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    let _ = tx.send(NetEvent::Stopped);
}

fn handle_association(stream: TcpStream, config: &ListenerConfig, tx: &Sender<NetEvent>) -> anyhow::Result<()> {
    let peer = stream.peer_addr().ok();
    let peer_addr = peer.map(|a| a.ip().to_string()).unwrap_or_default();
    let refused = Arc::new(std::sync::Mutex::new(None));
    let allow = Allowlist {
        entries: config.allow.lock().map(|l| l.clone()).unwrap_or_default(),
        peer: peer.map(|a| a.ip()),
        refused: refused.clone(),
    };

    let mut options = ServerAssociationOptions::new()
        .ae_access_control(allow)
        .ae_title(config.ae_title.clone())
        .promiscuous(config.promiscuous)
        .max_pdu_length(16384 * 8);
    for ts in TransferSyntaxRegistry.iter() {
        if !ts.is_unsupported() {
            options = options.with_transfer_syntax(ts.uid());
        }
    }
    for uid in STORAGE_ABSTRACT_SYNTAXES {
        options = options.with_abstract_syntax(*uid);
    }

    let mut association = match options.establish(stream) {
        Ok(a) => a,
        Err(e) => {
            let who = refused.lock().ok().and_then(|r| r.clone());
            if let Some(peer_ae) = who {
                tracing::warn!("refused association from {peer_ae} ({peer_addr}): not on the allow list");
                let _ = tx.send(NetEvent::Refused { peer_ae, peer_addr });
                return Ok(());
            }
            return Err(e.into());
        }
    };
    let peer_ae = association.peer_ae_title().to_string();
    let _ = tx.send(NetEvent::Association {
        peer_ae: peer_ae.clone(),
        peer_addr: peer_addr.clone(),
    });
    tracing::info!("association from {peer_ae} ({peer_addr})");

    let mut buffer: Vec<u8> = Vec::with_capacity(1024 * 1024);
    let mut msgid: u16 = 1;
    let mut sop_class_uid = String::new();
    let mut sop_instance_uid = String::new();
    let mut stored = 0usize;
    let command_ts = dicom_transfer_syntax_registry::entries::IMPLICIT_VR_LITTLE_ENDIAN.erased();

    loop {
        let pdu = match association.receive() {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("receive ended: {e}");
                break;
            }
        };
        match pdu {
            Pdu::PData { data } => {
                for value in data {
                    match (value.value_type, value.is_last) {
                        (PDataValueType::Data, false) => buffer.extend_from_slice(&value.data),
                        (PDataValueType::Command, true) => {
                            let cmd = InMemDicomObject::read_dataset_with_ts(value.data.as_slice(), &command_ts)?;
                            let field: u16 = cmd.element(tags::COMMAND_FIELD)?.to_int()?;
                            if field == 0x0030 {
                                let rq_id: u16 = cmd.element(tags::MESSAGE_ID)?.to_int()?;
                                let rsp = create_cecho_response(rq_id);
                                let mut bytes = Vec::new();
                                rsp.write_dataset_with_ts(&mut bytes, &command_ts)?;
                                association.send(&Pdu::PData {
                                    data: vec![PDataValue {
                                        presentation_context_id: value.presentation_context_id,
                                        value_type: PDataValueType::Command,
                                        is_last: true,
                                        data: bytes,
                                    }],
                                })?;
                                let _ = tx.send(NetEvent::Echo {
                                    peer_ae: peer_ae.clone(),
                                });
                            } else {
                                msgid = cmd.element(tags::MESSAGE_ID)?.to_int()?;
                                sop_class_uid = cmd
                                    .element(tags::AFFECTED_SOP_CLASS_UID)?
                                    .to_str()?
                                    .trim_end_matches('\0')
                                    .to_string();
                                sop_instance_uid = cmd
                                    .element(tags::AFFECTED_SOP_INSTANCE_UID)?
                                    .to_str()?
                                    .trim_end_matches('\0')
                                    .to_string();
                            }
                            buffer.clear();
                        }
                        (PDataValueType::Data, true) => {
                            buffer.extend_from_slice(&value.data);
                            let status = match store_instance(
                                &association,
                                value.presentation_context_id,
                                &buffer,
                                &sop_instance_uid,
                                &config.out_dir,
                            ) {
                                Ok(path) => {
                                    stored += 1;
                                    let _ = tx.send(NetEvent::Stored {
                                        path,
                                        sop_instance_uid: sop_instance_uid.clone(),
                                        peer_ae: peer_ae.clone(),
                                    });
                                    0x0000
                                }
                                Err(e) => {
                                    tracing::warn!("could not store instance {sop_instance_uid}: {e}");
                                    let _ = tx.send(NetEvent::Error(format!("store failed: {e}")));
                                    // Out of resources / processing failure.
                                    0xA700
                                }
                            };
                            buffer.clear();
                            let rsp = create_cstore_response(msgid, &sop_class_uid, &sop_instance_uid, status);
                            let mut bytes = Vec::new();
                            rsp.write_dataset_with_ts(&mut bytes, &command_ts)?;
                            association.send(&Pdu::PData {
                                data: vec![PDataValue {
                                    presentation_context_id: value.presentation_context_id,
                                    value_type: PDataValueType::Command,
                                    is_last: true,
                                    data: bytes,
                                }],
                            })?;
                        }
                        (PDataValueType::Command, false) => {
                            // Fragmented commands are rare; accumulate and
                            // let the final fragment be handled above.
                            buffer.extend_from_slice(&value.data);
                        }
                    }
                }
            }
            Pdu::ReleaseRQ => {
                let _ = association.send(&Pdu::ReleaseRP);
                break;
            }
            Pdu::AbortRQ { source } => {
                tracing::warn!("association aborted by peer: {source:?}");
                break;
            }
            _ => {}
        }
    }
    let _ = tx.send(NetEvent::Released { peer_ae, stored });
    Ok(())
}

fn store_instance<A: Association>(
    association: &A,
    pc_id: u8,
    data: &[u8],
    sop_instance_uid: &str,
    out_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let pc = association
        .presentation_contexts()
        .iter()
        .find(|pc| pc.id == pc_id)
        .ok_or_else(|| anyhow::anyhow!("missing presentation context {pc_id}"))?;
    write_instance(
        data,
        pc.transfer_syntax.trim_end_matches(' '),
        sop_instance_uid,
        out_dir,
    )
}

/// Write a received data set as a DICOM file in `out_dir`, through a
/// `.part` temporary so the incoming-folder scanner never sees half a
/// file. Used by the listener and by C-GET retrieval.
pub(crate) fn write_instance(
    data: &[u8],
    ts_uid: &str,
    sop_instance_uid: &str,
    out_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let ts_uid = ts_uid.to_string();
    let ts = TransferSyntaxRegistry
        .get(&ts_uid)
        .ok_or_else(|| anyhow::anyhow!("unknown transfer syntax {ts_uid}"))?;

    let obj = InMemDicomObject::read_dataset_with_ts(data, ts)?;
    let sop_class = obj
        .element(tags::SOP_CLASS_UID)?
        .to_str()?
        .trim_end_matches(' ')
        .to_string();
    let sop_instance = obj
        .element(tags::SOP_INSTANCE_UID)
        .ok()
        .and_then(|e| e.to_str().ok().map(|s| s.trim_end_matches(' ').to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| sop_instance_uid.to_string());

    let meta = FileMetaTableBuilder::new()
        .media_storage_sop_class_uid(sop_class)
        .media_storage_sop_instance_uid(sop_instance.clone())
        .transfer_syntax(ts_uid)
        .implementation_class_uid("1.2.826.0.1.3680043.10.1465.1")
        .implementation_version_name("RUTHOS_0_1")
        .build()?;
    let file = obj.with_exact_meta(meta);

    let name = safe_filename(&sop_instance);
    let final_path = out_dir.join(format!("{name}.dcm"));
    let part_path = out_dir.join(format!("{name}.part"));
    file.write_to_file(&part_path)?;
    std::fs::rename(&part_path, &final_path)?;
    Ok(final_path)
}

fn safe_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        format!("instance-{}", std::process::id())
    } else {
        cleaned
    }
}

fn create_cstore_response(
    message_id: u16,
    sop_class_uid: &str,
    sop_instance_uid: &str,
    status: u16,
) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        DataElement::new(tags::AFFECTED_SOP_CLASS_UID, VR::UI, dicom_value!(Str, sop_class_uid)),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [0x8001])),
        DataElement::new(
            tags::MESSAGE_ID_BEING_RESPONDED_TO,
            VR::US,
            dicom_value!(U16, [message_id]),
        ),
        DataElement::new(tags::COMMAND_DATA_SET_TYPE, VR::US, dicom_value!(U16, [0x0101])),
        DataElement::new(tags::STATUS, VR::US, dicom_value!(U16, [status])),
        DataElement::new(
            tags::AFFECTED_SOP_INSTANCE_UID,
            VR::UI,
            dicom_value!(Str, sop_instance_uid),
        ),
    ])
}

fn create_cecho_response(message_id: u16) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        DataElement::new(
            tags::AFFECTED_SOP_CLASS_UID,
            VR::UI,
            dicom_value!(Str, uids::VERIFICATION),
        ),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [0x8030])),
        DataElement::new(
            tags::MESSAGE_ID_BEING_RESPONDED_TO,
            VR::US,
            dicom_value!(U16, [message_id]),
        ),
        DataElement::new(tags::COMMAND_DATA_SET_TYPE, VR::US, dicom_value!(U16, [0x0101])),
        DataElement::new(tags::STATUS, VR::US, dicom_value!(U16, [0x0000])),
    ])
}

#[cfg(test)]
mod allow_tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> Option<IpAddr> {
        s.parse().ok()
    }

    #[test]
    fn addresses_and_subnets_match() {
        assert!(address_matches("", ip("10.0.0.1")));
        assert!(address_matches("192.168.1.20", ip("192.168.1.20")));
        assert!(!address_matches("192.168.1.20", ip("192.168.1.21")));
        assert!(address_matches("192.168.1.0/24", ip("192.168.1.200")));
        assert!(!address_matches("192.168.1.0/24", ip("192.168.2.1")));
        assert!(
            address_matches("192.168.1.20", ip("::ffff:192.168.1.20")),
            "IPv4 through IPv6"
        );
        assert!(address_matches("fe80::/16", ip("fe80::1")));
        assert!(!address_matches("not an address", ip("10.0.0.1")));
        assert!(!address_matches("10.0.0.1", None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_safe() {
        assert_eq!(safe_filename("1.2.3"), "1.2.3");
        assert_eq!(safe_filename("a/b\\c:d"), "a_b_c_d");
        assert!(safe_filename("").starts_with("instance-"));
    }
}
