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
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    Started { port: u16, ae_title: String },
    Association { peer_ae: String, peer_addr: String },
    Stored { path: PathBuf, sop_instance_uid: String, peer_ae: String },
    Echo { peer_ae: String },
    Released { peer_ae: String, stored: usize },
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
const STORAGE_ABSTRACT_SYNTAXES: &[&str] = &[
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
    let peer_addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();

    let mut options = ServerAssociationOptions::new()
        .accept_any()
        .ae_title(config.ae_title.clone())
        .promiscuous(true)
        .max_pdu_length(16384 * 8);
    for ts in TransferSyntaxRegistry.iter() {
        if !ts.is_unsupported() {
            options = options.with_transfer_syntax(ts.uid());
        }
    }
    for uid in STORAGE_ABSTRACT_SYNTAXES {
        options = options.with_abstract_syntax(*uid);
    }

    let mut association = options.establish(stream)?;
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
                                let _ = tx.send(NetEvent::Echo { peer_ae: peer_ae.clone() });
                            } else {
                                msgid = cmd.element(tags::MESSAGE_ID)?.to_int()?;
                                sop_class_uid = cmd.element(tags::AFFECTED_SOP_CLASS_UID)?.to_str()?.trim_end_matches('\0').to_string();
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
    let ts_uid = pc.transfer_syntax.trim_end_matches('\0').to_string();
    let ts = TransferSyntaxRegistry
        .get(&ts_uid)
        .ok_or_else(|| anyhow::anyhow!("unknown transfer syntax {ts_uid}"))?;

    let obj = InMemDicomObject::read_dataset_with_ts(data, ts)?;
    let sop_class = obj.element(tags::SOP_CLASS_UID)?.to_str()?.trim_end_matches('\0').to_string();
    let sop_instance = obj
        .element(tags::SOP_INSTANCE_UID)
        .ok()
        .and_then(|e| e.to_str().ok().map(|s| s.trim_end_matches('\0').to_string()))
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
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
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
        DataElement::new(tags::MESSAGE_ID_BEING_RESPONDED_TO, VR::US, dicom_value!(U16, [message_id])),
        DataElement::new(tags::COMMAND_DATA_SET_TYPE, VR::US, dicom_value!(U16, [0x0101])),
        DataElement::new(tags::STATUS, VR::US, dicom_value!(U16, [status])),
        DataElement::new(tags::AFFECTED_SOP_INSTANCE_UID, VR::UI, dicom_value!(Str, sop_instance_uid)),
    ])
}

fn create_cecho_response(message_id: u16) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        DataElement::new(tags::AFFECTED_SOP_CLASS_UID, VR::UI, dicom_value!(Str, uids::VERIFICATION)),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [0x8030])),
        DataElement::new(tags::MESSAGE_ID_BEING_RESPONDED_TO, VR::US, dicom_value!(U16, [message_id])),
        DataElement::new(tags::COMMAND_DATA_SET_TYPE, VR::US, dicom_value!(U16, [0x0101])),
        DataElement::new(tags::STATUS, VR::US, dicom_value!(U16, [0x0000])),
    ])
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
