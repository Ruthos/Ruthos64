//! Ruthos as a client of a PACS: echo, query (C-FIND), retrieve (C-GET or
//! C-MOVE) and send (C-STORE).
//!
//! The operations follow what Horos does through DCMTK: study-root queries
//! at study level, retrieval of whole studies, and one association per
//! send carrying every image. Retrieval tries C-GET first, which needs no
//! set-up on the PACS beyond knowing Ruthos's AE title, and falls back to
//! C-MOVE, which needs the PACS to know Ruthos's address and port.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dicom_core::{dicom_value, DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_encoding::transfer_syntax::TransferSyntaxIndex;
use dicom_object::{InMemDicomObject, OpenFileOptions};
use dicom_transfer_syntax_registry::TransferSyntaxRegistry;
use dicom_ul::association::client::{ClientAssociation, ClientAssociationOptions};

use crate::dimse::{
    self, command, context_for, decode, encode, is_cancel, is_pending, is_success, is_warning, priority,
    receive_message, send_message, status_element, str_of, u16_of, Link, Obj, Received, C_CANCEL_RQ, C_ECHO_RQ,
    C_ECHO_RSP, C_FIND_RQ, C_FIND_RSP, C_GET_RQ, C_GET_RSP, C_MOVE_RQ, C_MOVE_RSP, C_STORE_RQ, C_STORE_RSP,
    EXPLICIT_VR_LE, IMPLICIT_VR_LE,
};
use crate::error::{NetError, Node};

/// Time allowed to connect and between messages.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Transfer syntaxes Ruthos accepts when a PACS sends it images (C-GET):
/// uncompressed first, then every codec the viewer decodes.
pub const ACCEPTED_TRANSFER_SYNTAXES: &[&str] = &[
    EXPLICIT_VR_LE,
    IMPLICIT_VR_LE,
    uids::JPEG_BASELINE8_BIT,
    uids::JPEG_EXTENDED12_BIT,
    uids::JPEG_LOSSLESS_SV1,
    "1.2.840.10008.1.2.4.57",
    uids::JPEGLS_LOSSLESS,
    "1.2.840.10008.1.2.4.81",
    uids::JPEG2000_LOSSLESS,
    uids::JPEG2000,
    uids::RLE_LOSSLESS,
    uids::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN,
];

fn connect<'a>(
    options: ClientAssociationOptions<'a>,
    node: &Node,
    me_ae: &'a str,
) -> Result<ClientAssociation<std::net::TcpStream>, NetError> {
    options
        .calling_ae_title(me_ae)
        .called_ae_title(node.ae_title.clone())
        .max_pdu_length(65_536)
        .connection_timeout(TIMEOUT)
        .read_timeout(TIMEOUT)
        .write_timeout(TIMEOUT)
        .establish_with(&format!("{}:{}", node.host.trim(), node.port))
        .map_err(NetError::from_ul)
}

fn release(assoc: ClientAssociation<std::net::TcpStream>) {
    let _ = assoc.release();
}

// ------------------------------------------------------------------ echo

/// C-ECHO: does the PACS answer, and how fast.
pub fn echo(node: &Node, me_ae: &str) -> Result<Duration, NetError> {
    let start = Instant::now();
    let mut assoc = connect(
        ClientAssociationOptions::new().with_abstract_syntax(uids::VERIFICATION),
        node,
        me_ae,
    )?;
    let (pc, _) = context_for(&assoc, uids::VERIFICATION).ok_or(NetError::NotSupported("the echo test"))?;
    send_message(
        &mut assoc,
        pc,
        &command(C_ECHO_RQ, 1, uids::VERIFICATION, false, vec![]),
        None,
    )?;
    let result = match receive_message(&mut assoc)? {
        Received::Message(m) if m.field() == C_ECHO_RSP && is_success(m.status()) => Ok(start.elapsed()),
        Received::Message(m) => Err(NetError::Status(m.status())),
        Received::Release => Err(NetError::Aborted),
    };
    release(assoc);
    result
}

// ----------------------------------------------------------------- query

/// What to search for. Empty fields match anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StudyQuery {
    /// Patient name or ID: matched as a name (`*text*`) and as an ID.
    pub patient: String,
    pub accession: String,
    /// `YYYYMMDD`, `YYYYMMDD-`, `-YYYYMMDD` or `YYYYMMDD-YYYYMMDD`.
    pub study_date: String,
    /// One modality, such as `US`.
    pub modality: String,
    /// Search a specific study (to refresh one row).
    pub study_uid: String,
}

/// One study as the PACS describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StudyResult {
    pub study_uid: String,
    pub patient_name: String,
    pub patient_id: String,
    pub birth_date: String,
    pub sex: String,
    pub species: String,
    pub study_date: String,
    pub study_time: String,
    pub accession: String,
    pub description: String,
    pub modalities: String,
    pub referring: String,
    pub series: Option<u32>,
    pub instances: Option<u32>,
}

fn el(tag: Tag, vr: VR, value: &str) -> dicom_object::mem::InMemElement {
    if value.is_empty() {
        DataElement::new(tag, vr, PrimitiveValue::Empty)
    } else {
        DataElement::new(tag, vr, PrimitiveValue::from(value))
    }
}

fn study_identifier(q: &StudyQuery, name: &str, id: &str) -> Obj {
    let mut obj = InMemDicomObject::new_empty();
    for e in [
        el(tags::QUERY_RETRIEVE_LEVEL, VR::CS, "STUDY"),
        el(tags::PATIENT_NAME, VR::PN, name),
        el(tags::PATIENT_ID, VR::LO, id),
        el(tags::PATIENT_BIRTH_DATE, VR::DA, ""),
        el(tags::PATIENT_SEX, VR::CS, ""),
        el(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, ""),
        el(tags::STUDY_DATE, VR::DA, &q.study_date),
        el(tags::STUDY_TIME, VR::TM, ""),
        el(tags::ACCESSION_NUMBER, VR::SH, &q.accession),
        el(tags::STUDY_DESCRIPTION, VR::LO, ""),
        el(tags::MODALITIES_IN_STUDY, VR::CS, &q.modality),
        el(tags::REFERRING_PHYSICIAN_NAME, VR::PN, ""),
        el(tags::STUDY_INSTANCE_UID, VR::UI, &q.study_uid),
        el(tags::NUMBER_OF_STUDY_RELATED_SERIES, VR::IS, ""),
        el(tags::NUMBER_OF_STUDY_RELATED_INSTANCES, VR::IS, ""),
    ] {
        obj.put(e);
    }
    obj
}

fn number(obj: &Obj, tag: Tag) -> Option<u32> {
    str_of(obj, tag).trim().parse().ok()
}

fn study_result(obj: &Obj) -> StudyResult {
    StudyResult {
        study_uid: str_of(obj, tags::STUDY_INSTANCE_UID),
        patient_name: str_of(obj, tags::PATIENT_NAME),
        patient_id: str_of(obj, tags::PATIENT_ID),
        birth_date: str_of(obj, tags::PATIENT_BIRTH_DATE),
        sex: str_of(obj, tags::PATIENT_SEX),
        species: str_of(obj, tags::PATIENT_SPECIES_DESCRIPTION),
        study_date: str_of(obj, tags::STUDY_DATE),
        study_time: str_of(obj, tags::STUDY_TIME),
        accession: str_of(obj, tags::ACCESSION_NUMBER),
        description: str_of(obj, tags::STUDY_DESCRIPTION),
        modalities: str_of(obj, tags::MODALITIES_IN_STUDY).replace('\\', " "),
        referring: str_of(obj, tags::REFERRING_PHYSICIAN_NAME),
        series: number(obj, tags::NUMBER_OF_STUDY_RELATED_SERIES),
        instances: number(obj, tags::NUMBER_OF_STUDY_RELATED_INSTANCES),
    }
}

/// One C-FIND over an open association.
fn find_once(
    assoc: &mut ClientAssociation<std::net::TcpStream>,
    identifier: &Obj,
    message_id: u16,
    cancel: &AtomicBool,
) -> Result<Vec<Obj>, NetError> {
    let model = uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND;
    let (pc, ts) = context_for(assoc, model).ok_or(NetError::NotSupported("searching (C-FIND)"))?;
    let data = encode(identifier, &ts)?;
    send_message(
        assoc,
        pc,
        &command(C_FIND_RQ, message_id, model, true, vec![priority()]),
        Some(&data),
    )?;
    let mut out = Vec::new();
    let mut cancelled = false;
    loop {
        if cancel.load(Ordering::Relaxed) && !cancelled {
            send_cancel(assoc, pc, message_id)?;
            cancelled = true;
        }
        let Received::Message(m) = receive_message(assoc)? else {
            return Err(NetError::Aborted);
        };
        if m.field() != C_FIND_RSP {
            continue;
        }
        let status = m.status();
        if is_pending(status) {
            if let Some(d) = &m.data {
                out.push(decode(d, &ts)?);
            }
            continue;
        }
        if is_success(status) || is_warning(status) {
            return Ok(out);
        }
        if is_cancel(status) {
            return Err(NetError::Cancelled);
        }
        return Err(NetError::Status(status));
    }
}

fn send_cancel(link: &mut dyn Link, pc: u8, message_id: u16) -> Result<(), NetError> {
    let cancel = InMemDicomObject::command_from_element_iter([
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [C_CANCEL_RQ])),
        DataElement::new(
            tags::MESSAGE_ID_BEING_RESPONDED_TO,
            VR::US,
            dicom_value!(U16, [message_id]),
        ),
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [dimse::NO_DATA_SET]),
        ),
    ]);
    send_message(link, pc, &cancel, None)
}

/// One series of a study as the PACS describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeriesResult {
    pub series_uid: String,
    pub number: Option<i64>,
    pub modality: String,
    pub description: String,
    pub instances: Option<u32>,
}

/// The series of a study (a C-FIND at series level), in series order.
pub fn find_series(
    node: &Node,
    me_ae: &str,
    study_uid: &str,
    cancel: &AtomicBool,
) -> Result<Vec<SeriesResult>, NetError> {
    let mut assoc = connect(
        ClientAssociationOptions::new().with_presentation_context(
            uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND,
            vec![EXPLICIT_VR_LE, IMPLICIT_VR_LE],
        ),
        node,
        me_ae,
    )?;
    let mut q = InMemDicomObject::new_empty();
    for e in [
        el(tags::QUERY_RETRIEVE_LEVEL, VR::CS, "SERIES"),
        el(tags::STUDY_INSTANCE_UID, VR::UI, study_uid),
        el(tags::SERIES_INSTANCE_UID, VR::UI, ""),
        el(tags::SERIES_NUMBER, VR::IS, ""),
        el(tags::MODALITY, VR::CS, ""),
        el(tags::SERIES_DESCRIPTION, VR::LO, ""),
        el(tags::NUMBER_OF_SERIES_RELATED_INSTANCES, VR::IS, ""),
    ] {
        q.put(e);
    }
    let found = find_once(&mut assoc, &q, 1, cancel);
    release(assoc);
    let mut out: Vec<SeriesResult> = found?
        .iter()
        .map(|o| SeriesResult {
            series_uid: str_of(o, tags::SERIES_INSTANCE_UID),
            number: str_of(o, tags::SERIES_NUMBER).trim().parse().ok(),
            modality: str_of(o, tags::MODALITY),
            description: str_of(o, tags::SERIES_DESCRIPTION),
            instances: number(o, tags::NUMBER_OF_SERIES_RELATED_INSTANCES),
        })
        .collect();
    out.sort_by_key(|s| s.number.unwrap_or(i64::MAX));
    Ok(out)
}

/// Search the PACS for studies. A patient field is tried as a name and as
/// an ID, and the results merged, so the user need not say which it is.
pub fn find_studies(
    node: &Node,
    me_ae: &str,
    q: &StudyQuery,
    cancel: &AtomicBool,
) -> Result<Vec<StudyResult>, NetError> {
    let ts = vec![EXPLICIT_VR_LE, IMPLICIT_VR_LE];
    let mut assoc = connect(
        ClientAssociationOptions::new()
            .with_presentation_context(uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND, ts),
        node,
        me_ae,
    )?;
    let text = q.patient.trim();
    let mut results = if text.is_empty() {
        find_once(&mut assoc, &study_identifier(q, "", ""), 1, cancel)?
            .iter()
            .map(study_result)
            .collect::<Vec<_>>()
    } else {
        let name = if text.contains('*') {
            text.to_string()
        } else {
            format!("*{text}*")
        };
        let mut r = find_once(&mut assoc, &study_identifier(q, &name, ""), 1, cancel)?
            .iter()
            .map(study_result)
            .collect::<Vec<_>>();
        if !text.contains(' ') {
            for s in find_once(&mut assoc, &study_identifier(q, "", text), 2, cancel)?
                .iter()
                .map(study_result)
                .collect::<Vec<_>>()
            {
                if !r.iter().any(|x| x.study_uid == s.study_uid) {
                    r.push(s);
                }
            }
        }
        r
    };
    release(assoc);
    // Newest first, as the database window sorts.
    results.sort_by(|a, b| {
        (b.study_date.as_str(), b.study_time.as_str()).cmp(&(a.study_date.as_str(), a.study_time.as_str()))
    });
    Ok(results)
}

// -------------------------------------------------------------- retrieve

/// How to bring a study from the PACS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrieveMethod {
    /// C-GET, falling back to C-MOVE when the PACS does not offer C-GET.
    Auto,
    /// The PACS sends the images back over the same connection.
    Get,
    /// The PACS opens its own connection to Ruthos's listener.
    Move,
}

/// Progress of a retrieval, from the PACS's sub-operation counts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RetrieveProgress {
    pub received: u32,
    pub remaining: Option<u32>,
    pub completed: u32,
    pub failed: u32,
    pub warning: u32,
}

impl RetrieveProgress {
    pub fn total(&self) -> Option<u32> {
        self.remaining.map(|r| r + self.completed + self.failed + self.warning)
    }

    fn update(&mut self, cmd: &Obj) {
        if cmd.element(tags::NUMBER_OF_REMAINING_SUBOPERATIONS).is_ok() {
            self.remaining = Some(u16_of(cmd, tags::NUMBER_OF_REMAINING_SUBOPERATIONS) as u32);
        }
        self.completed = self
            .completed
            .max(u16_of(cmd, tags::NUMBER_OF_COMPLETED_SUBOPERATIONS) as u32);
        self.failed = self
            .failed
            .max(u16_of(cmd, tags::NUMBER_OF_FAILED_SUBOPERATIONS) as u32);
        self.warning = self
            .warning
            .max(u16_of(cmd, tags::NUMBER_OF_WARNING_SUBOPERATIONS) as u32);
    }
}

/// How a retrieval ended.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrieveOutcome {
    pub method: RetrieveMethod,
    pub progress: RetrieveProgress,
    /// Files written by C-GET (C-MOVE files arrive through the listener).
    pub files: Vec<PathBuf>,
}

fn retrieve_identifier(study_uid: &str, series_uid: Option<&str>) -> Obj {
    let mut obj = InMemDicomObject::new_empty();
    obj.put(el(
        tags::QUERY_RETRIEVE_LEVEL,
        VR::CS,
        if series_uid.is_some() { "SERIES" } else { "STUDY" },
    ));
    obj.put(el(tags::STUDY_INSTANCE_UID, VR::UI, study_uid));
    if let Some(series) = series_uid {
        obj.put(el(tags::SERIES_INSTANCE_UID, VR::UI, series));
    }
    obj
}

/// Bring one series of a study into `out_dir`.
#[allow(clippy::too_many_arguments)]
pub fn retrieve_series(
    node: &Node,
    me_ae: &str,
    study_uid: &str,
    series_uid: &str,
    method: RetrieveMethod,
    out_dir: &Path,
    progress: &dyn Fn(RetrieveProgress),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    retrieve_with(
        node,
        me_ae,
        &retrieve_identifier(study_uid, Some(series_uid)),
        method,
        out_dir,
        progress,
        cancel,
    )
}

/// Bring a study into `out_dir` (the archive's incoming folder).
pub fn retrieve_study(
    node: &Node,
    me_ae: &str,
    study_uid: &str,
    method: RetrieveMethod,
    out_dir: &Path,
    progress: &dyn Fn(RetrieveProgress),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    retrieve_with(
        node,
        me_ae,
        &retrieve_identifier(study_uid, None),
        method,
        out_dir,
        progress,
        cancel,
    )
}

fn retrieve_with(
    node: &Node,
    me_ae: &str,
    identifier: &Obj,
    method: RetrieveMethod,
    out_dir: &Path,
    progress: &dyn Fn(RetrieveProgress),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    match method {
        RetrieveMethod::Get => get_study(node, me_ae, identifier, out_dir, progress, cancel),
        RetrieveMethod::Move => move_study(node, me_ae, identifier, progress, cancel),
        RetrieveMethod::Auto => match get_study(node, me_ae, identifier, out_dir, progress, cancel) {
            Err(NetError::NotSupported(_)) | Err(NetError::Status(0xA900..=0xA9FF)) => {
                tracing::info!("{} does not offer C-GET; retrieving with C-MOVE", node.ae_title);
                move_study(node, me_ae, identifier, progress, cancel)
            }
            other => other,
        },
    }
}

fn get_study(
    node: &Node,
    me_ae: &str,
    identifier: &Obj,
    out_dir: &Path,
    progress: &dyn Fn(RetrieveProgress),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    let model = uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_GET;
    let mut options =
        ClientAssociationOptions::new().with_presentation_context(model, vec![EXPLICIT_VR_LE, IMPLICIT_VR_LE]);
    // For C-GET Ruthos plays the storage role on the same association.
    for sop in crate::scp::STORAGE_ABSTRACT_SYNTAXES
        .iter()
        .filter(|s| **s != uids::VERIFICATION)
    {
        options = options
            .with_presentation_context(*sop, ACCEPTED_TRANSFER_SYNTAXES.to_vec())
            .with_role_selection(*sop, false, true);
    }
    let mut assoc = connect(options, node, me_ae)?;
    let (pc, ts) = context_for(&assoc, model).ok_or(NetError::NotSupported("retrieval by C-GET"))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|e| NetError::Other(format!("cannot write to {}: {e}", out_dir.display())))?;
    let data = encode(identifier, &ts)?;
    send_message(
        &mut assoc,
        pc,
        &command(C_GET_RQ, 1, model, true, vec![priority()]),
        Some(&data),
    )?;

    let mut state = RetrieveProgress::default();
    let mut files = Vec::new();
    let mut cancelled = false;
    let result = loop {
        if cancel.load(Ordering::Relaxed) && !cancelled {
            send_cancel(&mut assoc, pc, 1)?;
            cancelled = true;
        }
        let Received::Message(m) = receive_message(&mut assoc)? else {
            break Err(NetError::Aborted);
        };
        match m.field() {
            C_STORE_RQ => {
                let sop_class = str_of(&m.command, tags::AFFECTED_SOP_CLASS_UID);
                let sop_instance = str_of(&m.command, tags::AFFECTED_SOP_INSTANCE_UID);
                let msg_id = u16_of(&m.command, tags::MESSAGE_ID);
                let store_ts = dimse::transfer_syntax_of(&assoc, m.pc_id).unwrap_or_else(|| IMPLICIT_VR_LE.into());
                let status = match m
                    .data
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("no data set"))
                    .and_then(|d| crate::scp::write_instance(d, &store_ts, &sop_instance, out_dir))
                {
                    Ok(path) => {
                        files.push(path);
                        state.received += 1;
                        progress(state);
                        0x0000
                    }
                    Err(e) => {
                        tracing::warn!("could not store {sop_instance}: {e}");
                        0xA700
                    }
                };
                let rsp = command(
                    C_STORE_RSP,
                    msg_id,
                    &sop_class,
                    false,
                    vec![
                        status_element(status),
                        DataElement::new(
                            tags::AFFECTED_SOP_INSTANCE_UID,
                            VR::UI,
                            PrimitiveValue::from(sop_instance.as_str()),
                        ),
                    ],
                );
                send_message(&mut assoc, m.pc_id, &rsp, None)?;
            }
            C_GET_RSP => {
                state.update(&m.command);
                progress(state);
                let status = m.status();
                if is_pending(status) {
                    continue;
                }
                if is_success(status) || is_warning(status) {
                    break Ok(());
                }
                if is_cancel(status) {
                    break Err(NetError::Cancelled);
                }
                break Err(NetError::Status(status));
            }
            _ => {}
        }
    };
    release(assoc);
    result.map(|()| RetrieveOutcome {
        method: RetrieveMethod::Get,
        progress: state,
        files,
    })
}

fn move_study(
    node: &Node,
    me_ae: &str,
    identifier: &Obj,
    progress: &dyn Fn(RetrieveProgress),
    cancel: &AtomicBool,
) -> Result<RetrieveOutcome, NetError> {
    let model = uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_MOVE;
    let mut assoc = connect(
        ClientAssociationOptions::new().with_presentation_context(model, vec![EXPLICIT_VR_LE, IMPLICIT_VR_LE]),
        node,
        me_ae,
    )?;
    let (pc, ts) = context_for(&assoc, model).ok_or(NetError::NotSupported("retrieval by C-MOVE"))?;
    let data = encode(identifier, &ts)?;
    let destination = DataElement::new(tags::MOVE_DESTINATION, VR::AE, PrimitiveValue::from(me_ae));
    send_message(
        &mut assoc,
        pc,
        &command(C_MOVE_RQ, 1, model, true, vec![priority(), destination]),
        Some(&data),
    )?;
    let mut state = RetrieveProgress::default();
    let mut cancelled = false;
    let result = loop {
        if cancel.load(Ordering::Relaxed) && !cancelled {
            send_cancel(&mut assoc, pc, 1)?;
            cancelled = true;
        }
        let Received::Message(m) = receive_message(&mut assoc)? else {
            break Err(NetError::Aborted);
        };
        if m.field() != C_MOVE_RSP {
            continue;
        }
        state.update(&m.command);
        progress(state);
        let status = m.status();
        if is_pending(status) {
            continue;
        }
        if is_success(status) || is_warning(status) {
            break Ok(());
        }
        if is_cancel(status) {
            break Err(NetError::Cancelled);
        }
        break Err(if status == 0xA801 {
            NetError::MoveDestinationUnknown
        } else {
            NetError::Status(status)
        });
    };
    release(assoc);
    result.map(|()| RetrieveOutcome {
        method: RetrieveMethod::Move,
        progress: state,
        files: Vec::new(),
    })
}

// ------------------------------------------------------------------ send

/// How a send ended: how many images arrived, and why any did not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SendOutcome {
    pub sent: usize,
    pub failed: Vec<(PathBuf, String)>,
}

fn is_uncompressed(ts: &str) -> bool {
    matches!(ts, IMPLICIT_VR_LE | EXPLICIT_VR_LE | "1.2.840.10008.1.2.2")
}

/// Send files to the PACS over one association. Each image goes in its own
/// transfer syntax when the PACS accepts it; uncompressed images are
/// re-encoded when the PACS wants another uncompressed syntax.
pub fn send_files(
    node: &Node,
    me_ae: &str,
    files: &[PathBuf],
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<SendOutcome, NetError> {
    // What is being sent: SOP class and transfer syntax of each file.
    let mut items: Vec<(PathBuf, String, String)> = Vec::new();
    let mut outcome = SendOutcome::default();
    for f in files {
        match OpenFileOptions::new().read_until(tags::PIXEL_DATA).open_file(f) {
            Ok(obj) => {
                let meta = obj.meta();
                items.push((
                    f.clone(),
                    meta.media_storage_sop_class_uid.trim_end_matches('\0').to_string(),
                    meta.transfer_syntax.trim_end_matches('\0').to_string(),
                ));
            }
            Err(e) => outcome.failed.push((f.clone(), format!("cannot be read: {e}"))),
        }
    }
    if items.is_empty() {
        return Ok(outcome);
    }
    // One proposal per image type and format as stored, so a PACS that
    // takes the format gets the image unchanged, and one uncompressed
    // proposal per image type as the fallback.
    let mut contexts: Vec<(String, Vec<String>)> = Vec::new();
    for (_, sop, ts) in &items {
        if !is_uncompressed(ts) && !contexts.iter().any(|(s, t)| s == sop && t.len() == 1 && t[0] == *ts) {
            contexts.push((sop.clone(), vec![ts.clone()]));
        }
        if !contexts.iter().any(|(s, t)| s == sop && t.len() > 1) {
            contexts.push((sop.clone(), vec![EXPLICIT_VR_LE.into(), IMPLICIT_VR_LE.into()]));
        }
    }
    let mut options = ClientAssociationOptions::new();
    for (sop, ts) in contexts.iter().take(127) {
        options = options.with_presentation_context(sop.clone(), ts.clone());
    }
    let mut assoc = connect(options, node, me_ae)?;
    let total = items.len();
    for (i, (path, sop, file_ts)) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            release(assoc);
            return Err(NetError::Cancelled);
        }
        let accepted = assoc.contexts();
        let exact = accepted.iter().find(|(_, a, t)| a == sop && t == file_ts);
        let plain = accepted.iter().find(|(_, a, t)| a == sop && is_uncompressed(t));
        let Some((pc, ts)) = exact.or(plain).map(|(id, _, t)| (*id, t.clone())) else {
            outcome
                .failed
                .push((path.clone(), "the PACS does not accept this kind of image".into()));
            continue;
        };
        let mut obj = match dicom_object::open_file(path) {
            Ok(o) => o,
            Err(e) => {
                outcome.failed.push((path.clone(), format!("cannot be read: {e}")));
                continue;
            }
        };
        if ts != *file_ts && !is_uncompressed(file_ts) {
            // The PACS takes only uncompressed images of this type.
            use dicom_pixeldata::Transcode as _;
            let Some(target) = TransferSyntaxRegistry.get(&ts) else {
                outcome
                    .failed
                    .push((path.clone(), format!("unknown transfer syntax {ts}")));
                continue;
            };
            if let Err(e) = obj.transcode(target) {
                outcome.failed.push((
                    path.clone(),
                    format!(
                        "the PACS accepts only {} for this kind of image, and this compressed one could not be converted ({e})",
                        ts_name(&ts)
                    ),
                ));
                continue;
            }
        }
        let sop_instance = obj
            .meta()
            .media_storage_sop_instance_uid
            .trim_end_matches('\0')
            .to_string();
        let data = encode(&obj.into_inner(), &ts)?;
        let cmd = command(
            C_STORE_RQ,
            (i % 65_000 + 1) as u16,
            sop,
            true,
            vec![
                priority(),
                DataElement::new(
                    tags::AFFECTED_SOP_INSTANCE_UID,
                    VR::UI,
                    PrimitiveValue::from(sop_instance.as_str()),
                ),
            ],
        );
        send_message(&mut assoc, pc, &cmd, Some(&data))?;
        match receive_message(&mut assoc)? {
            Received::Message(m) if m.field() == C_STORE_RSP => {
                let s = m.status();
                if is_success(s) || is_warning(s) {
                    outcome.sent += 1;
                } else {
                    outcome.failed.push((path.clone(), crate::error::status_words(s)));
                }
            }
            Received::Message(_) => outcome.failed.push((path.clone(), "unexpected reply".into())),
            Received::Release => return Err(NetError::Aborted),
        }
        progress(i + 1, total);
    }
    release(assoc);
    Ok(outcome)
}

fn ts_name(uid: &str) -> &'static str {
    match uid {
        IMPLICIT_VR_LE => "uncompressed (implicit VR)",
        EXPLICIT_VR_LE => "uncompressed (explicit VR)",
        uids::JPEG_BASELINE8_BIT => "JPEG baseline",
        uids::JPEG_LOSSLESS_SV1 => "JPEG lossless",
        uids::JPEG2000 => "JPEG 2000",
        uids::JPEG2000_LOSSLESS => "JPEG 2000 lossless",
        uids::RLE_LOSSLESS => "RLE",
        _ => "another format",
    }
}
