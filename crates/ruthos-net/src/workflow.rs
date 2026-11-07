//! Modality Worklist and Storage Commitment, as a client.
//!
//! A worklist lists the procedures a practice has scheduled, so pictures and
//! studies made in Ruthos can carry the patient and accession the practice
//! system gave them. Storage Commitment asks a PACS to confirm that it holds
//! what was sent, safely enough to delete a local copy. The PACS answers with
//! an N-EVENT-REPORT, on the same association or later on a new one to the
//! listener; both are read here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{dicom_value, DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_ul::association::client::ClientAssociationOptions;

use crate::dimse::{
    context_for, decode, encode, is_cancel, is_pending, is_success, is_warning, priority, receive_message,
    send_message, status_element, str_of, u16_of, Link, Obj, Received, EXPLICIT_VR_LE, IMPLICIT_VR_LE,
};
use crate::error::{NetError, Node};
use crate::scu::connect;

/// Modality Worklist Information Model - FIND.
pub const WORKLIST_FIND: &str = "1.2.840.10008.5.1.4.31";
/// Storage Commitment Push Model.
pub const STORAGE_COMMITMENT: &str = "1.2.840.10008.1.20.1";
/// The one well-known instance of Storage Commitment Push Model.
pub const COMMITMENT_INSTANCE: &str = "1.2.840.10008.1.20.1.1";

pub const N_EVENT_REPORT_RQ: u16 = 0x0100;
pub const N_EVENT_REPORT_RSP: u16 = 0x8100;
pub const N_ACTION_RQ: u16 = 0x0130;
pub const N_ACTION_RSP: u16 = 0x8130;

fn el(tag: Tag, vr: VR, value: &str) -> dicom_object::mem::InMemElement {
    DataElement::new(tag, vr, PrimitiveValue::from(value))
}

fn sequence(tag: Tag, items: Vec<Obj>) -> dicom_object::mem::InMemElement {
    DataElement::new(
        tag,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED)),
    )
}

// -------------------------------------------------------------- worklist

/// What to ask the worklist for. Empty fields match anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorklistQuery {
    /// `YYYYMMDD`, or a range `YYYYMMDD-YYYYMMDD`.
    pub date: String,
    pub modality: String,
    /// The station the work is scheduled on, as its AE title.
    pub station: String,
    /// Part of the patient's name.
    pub patient: String,
}

/// One scheduled procedure.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkItem {
    pub patient_name: String,
    pub patient_id: String,
    pub birth_date: String,
    pub sex: String,
    pub species: String,
    pub breed: String,
    pub owner: String,
    pub accession: String,
    pub study_uid: String,
    pub referring: String,
    pub procedure: String,
    pub modality: String,
    pub station: String,
    /// `YYYYMMDD` and `HHMMSS` as the worklist gives them.
    pub date: String,
    pub time: String,
    pub step: String,
}

fn worklist_identifier(q: &WorklistQuery) -> Obj {
    let mut step = InMemDicomObject::new_empty();
    for e in [
        el(tags::SCHEDULED_STATION_AE_TITLE, VR::AE, q.station.trim()),
        el(tags::SCHEDULED_PROCEDURE_STEP_START_DATE, VR::DA, q.date.trim()),
        el(tags::SCHEDULED_PROCEDURE_STEP_START_TIME, VR::TM, ""),
        el(tags::MODALITY, VR::CS, q.modality.trim()),
        el(tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION, VR::LO, ""),
        el(tags::SCHEDULED_PROCEDURE_STEP_ID, VR::SH, ""),
    ] {
        step.put(e);
    }
    let mut obj = InMemDicomObject::new_empty();
    let name = match q.patient.trim() {
        "" => String::new(),
        p => format!("*{p}*"),
    };
    for e in [
        el(tags::PATIENT_NAME, VR::PN, &name),
        el(tags::PATIENT_ID, VR::LO, ""),
        el(tags::PATIENT_BIRTH_DATE, VR::DA, ""),
        el(tags::PATIENT_SEX, VR::CS, ""),
        el(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, ""),
        el(tags::PATIENT_BREED_DESCRIPTION, VR::LO, ""),
        el(tags::RESPONSIBLE_PERSON, VR::PN, ""),
        el(tags::ACCESSION_NUMBER, VR::SH, ""),
        el(tags::STUDY_INSTANCE_UID, VR::UI, ""),
        el(tags::REFERRING_PHYSICIAN_NAME, VR::PN, ""),
        el(tags::REQUESTED_PROCEDURE_DESCRIPTION, VR::LO, ""),
        el(tags::REQUESTED_PROCEDURE_ID, VR::SH, ""),
    ] {
        obj.put(e);
    }
    obj.put(sequence(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![step]));
    obj
}

fn work_item(obj: &Obj) -> WorkItem {
    let step = obj
        .element(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)
        .ok()
        .and_then(|e| e.items().and_then(|items| items.first().cloned()))
        .unwrap_or_else(InMemDicomObject::new_empty);
    WorkItem {
        patient_name: str_of(obj, tags::PATIENT_NAME),
        patient_id: str_of(obj, tags::PATIENT_ID),
        birth_date: str_of(obj, tags::PATIENT_BIRTH_DATE),
        sex: str_of(obj, tags::PATIENT_SEX),
        species: str_of(obj, tags::PATIENT_SPECIES_DESCRIPTION),
        breed: str_of(obj, tags::PATIENT_BREED_DESCRIPTION),
        owner: str_of(obj, tags::RESPONSIBLE_PERSON),
        accession: str_of(obj, tags::ACCESSION_NUMBER),
        study_uid: str_of(obj, tags::STUDY_INSTANCE_UID),
        referring: str_of(obj, tags::REFERRING_PHYSICIAN_NAME),
        procedure: str_of(obj, tags::REQUESTED_PROCEDURE_DESCRIPTION),
        modality: str_of(&step, tags::MODALITY),
        station: str_of(&step, tags::SCHEDULED_STATION_AE_TITLE),
        date: str_of(&step, tags::SCHEDULED_PROCEDURE_STEP_START_DATE),
        time: str_of(&step, tags::SCHEDULED_PROCEDURE_STEP_START_TIME),
        step: str_of(&step, tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION),
    }
}

/// Ask a worklist server what is scheduled, soonest first.
pub fn find_worklist(
    node: &Node,
    me_ae: &str,
    query: &WorklistQuery,
    cancel: &AtomicBool,
) -> Result<Vec<WorkItem>, NetError> {
    let mut assoc = connect(
        ClientAssociationOptions::new().with_abstract_syntax(WORKLIST_FIND),
        node,
        me_ae,
    )?;
    let (pc, ts) = context_for(&assoc, WORKLIST_FIND).ok_or(NetError::NotSupported("a worklist"))?;
    let data = encode(&worklist_identifier(query), &ts)?;
    send_message(
        &mut assoc,
        pc,
        &crate::dimse::command(crate::dimse::C_FIND_RQ, 1, WORKLIST_FIND, true, vec![priority()]),
        Some(&data),
    )?;
    let mut out = Vec::new();
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err(NetError::Cancelled);
        }
        let Received::Message(m) = receive_message(&mut assoc)? else {
            break Err(NetError::Aborted);
        };
        if m.field() != crate::dimse::C_FIND_RSP {
            continue;
        }
        let status = m.status();
        if is_pending(status) {
            if let Some(d) = &m.data {
                out.push(work_item(&decode(d, &ts)?));
            }
        } else if is_success(status) || is_warning(status) {
            break Ok(());
        } else if is_cancel(status) {
            break Err(NetError::Cancelled);
        } else {
            break Err(NetError::Status(status));
        }
    };
    let _ = assoc.release();
    result?;
    out.sort_by(|a, b| (a.date.as_str(), a.time.as_str()).cmp(&(b.date.as_str(), b.time.as_str())));
    Ok(out)
}

// ------------------------------------------------------ storage commitment

/// A PACS's answer about what it holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Commitment {
    pub transaction_uid: String,
    /// SOP Instance UIDs it holds.
    pub committed: Vec<String>,
    /// SOP Instance UIDs it does not, each with its reason code.
    pub failed: Vec<(String, u16)>,
}

impl Commitment {
    /// In words, for the status line.
    pub fn summary(&self) -> String {
        match (self.committed.len(), self.failed.len()) {
            (n, 0) => format!("The PACS confirms it holds all {n} images sent."),
            (n, f) => format!(
                "The PACS holds {n} of the images sent and not {f}: send {} again.",
                if f == 1 { "it" } else { "them" }
            ),
        }
    }
}

/// How asking for commitment ended.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome {
    Answered(Commitment),
    /// The PACS took the request and will answer on a new association to the
    /// listener, under this transaction UID.
    Later(String),
}

fn referenced(items: &[(String, String)]) -> Vec<Obj> {
    items
        .iter()
        .map(|(class, instance)| {
            let mut item = InMemDicomObject::new_empty();
            item.put(el(tags::REFERENCED_SOP_CLASS_UID, VR::UI, class));
            item.put(el(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, instance));
            item
        })
        .collect()
}

/// The command of an N-ACTION request for commitment.
fn action_command(message_id: u16) -> Obj {
    InMemDicomObject::command_from_element_iter([
        el(tags::REQUESTED_SOP_CLASS_UID, VR::UI, STORAGE_COMMITMENT),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [N_ACTION_RQ])),
        DataElement::new(tags::MESSAGE_ID, VR::US, dicom_value!(U16, [message_id])),
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [crate::dimse::HAS_DATA_SET]),
        ),
        el(tags::REQUESTED_SOP_INSTANCE_UID, VR::UI, COMMITMENT_INSTANCE),
        DataElement::new(tags::ACTION_TYPE_ID, VR::US, dicom_value!(U16, [1])),
    ])
}

/// The data set of an N-ACTION request for commitment.
pub fn commitment_request(transaction_uid: &str, items: &[(String, String)]) -> Obj {
    let mut obj = InMemDicomObject::new_empty();
    obj.put(el(tags::TRANSACTION_UID, VR::UI, transaction_uid));
    obj.put(sequence(tags::REFERENCED_SOP_SEQUENCE, referenced(items)));
    obj
}

/// A commitment from the data set of an N-EVENT-REPORT.
pub fn commitment_from(obj: &Obj) -> Commitment {
    let list = |tag: Tag| -> Vec<Obj> {
        obj.element(tag)
            .ok()
            .and_then(|e| e.items().map(|i| i.to_vec()))
            .unwrap_or_default()
    };
    Commitment {
        transaction_uid: str_of(obj, tags::TRANSACTION_UID),
        committed: list(tags::REFERENCED_SOP_SEQUENCE)
            .iter()
            .map(|i| str_of(i, tags::REFERENCED_SOP_INSTANCE_UID))
            .collect(),
        failed: list(tags::FAILED_SOP_SEQUENCE)
            .iter()
            .map(|i| {
                (
                    str_of(i, tags::REFERENCED_SOP_INSTANCE_UID),
                    u16_of(i, tags::FAILURE_REASON),
                )
            })
            .collect(),
    }
}

/// The reply to an N-EVENT-REPORT.
pub fn event_report_response(message_id: u16, event_type: u16) -> Obj {
    InMemDicomObject::command_from_element_iter([
        el(tags::AFFECTED_SOP_CLASS_UID, VR::UI, STORAGE_COMMITMENT),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [N_EVENT_REPORT_RSP])),
        DataElement::new(
            tags::MESSAGE_ID_BEING_RESPONDED_TO,
            VR::US,
            dicom_value!(U16, [message_id]),
        ),
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [crate::dimse::NO_DATA_SET]),
        ),
        status_element(0),
        el(tags::AFFECTED_SOP_INSTANCE_UID, VR::UI, COMMITMENT_INSTANCE),
        DataElement::new(tags::EVENT_TYPE_ID, VR::US, dicom_value!(U16, [event_type])),
    ])
}

/// Ask a PACS to confirm it holds these images, each a SOP Class and SOP
/// Instance UID. Waits up to `wait` for the answer on the same association;
/// a PACS that answers later does so to the listener.
pub fn request_commitment(
    node: &Node,
    me_ae: &str,
    transaction_uid: &str,
    items: &[(String, String)],
    wait: Duration,
) -> Result<CommitOutcome, NetError> {
    let options = ClientAssociationOptions::new()
        .with_presentation_context(STORAGE_COMMITMENT, vec![EXPLICIT_VR_LE, IMPLICIT_VR_LE]);
    let mut assoc = connect(options, node, me_ae)?;
    let (pc, ts) =
        context_for(&assoc, STORAGE_COMMITMENT).ok_or(NetError::NotSupported("confirming what it stored"))?;
    let data = encode(&commitment_request(transaction_uid, items), &ts)?;
    send_message(&mut assoc, pc, &action_command(1), Some(&data))?;
    match receive_message(&mut assoc)? {
        Received::Message(m) if m.field() == N_ACTION_RSP && is_success(m.status()) => {}
        Received::Message(m) => {
            let _ = assoc.release();
            return Err(NetError::Status(m.status()));
        }
        Received::Release => return Err(NetError::Aborted),
    }
    let outcome = wait_for_report(&mut assoc, &ts, wait);
    let _ = assoc.release();
    Ok(outcome.unwrap_or_else(|| CommitOutcome::Later(transaction_uid.to_string())))
}

/// An N-EVENT-REPORT on this association, answered, if one comes in time.
fn wait_for_report(link: &mut dyn Link, ts: &str, wait: Duration) -> Option<CommitOutcome> {
    let until = std::time::Instant::now() + wait;
    while std::time::Instant::now() < until {
        let Ok(Received::Message(m)) = receive_message(link) else {
            return None;
        };
        if m.field() != N_EVENT_REPORT_RQ {
            continue;
        }
        let commitment = commitment_from(&decode(m.data.as_deref()?, ts).ok()?);
        let rsp = event_report_response(
            u16_of(&m.command, tags::MESSAGE_ID),
            u16_of(&m.command, tags::EVENT_TYPE_ID),
        );
        let _ = send_message(link, m.pc_id, &rsp, None);
        return Some(CommitOutcome::Answered(commitment));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commitment_report_reads_both_lists() {
        let items = vec![("1.2.3".to_string(), "1.2.3.4".to_string())];
        let mut report = commitment_request("9.9.9", &items);
        let mut failed = InMemDicomObject::new_empty();
        failed.put(el(tags::REFERENCED_SOP_CLASS_UID, VR::UI, "1.2.3"));
        failed.put(el(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, "1.2.3.5"));
        failed.put(DataElement::new(
            tags::FAILURE_REASON,
            VR::US,
            dicom_value!(U16, [0x0112]),
        ));
        report.put(sequence(tags::FAILED_SOP_SEQUENCE, vec![failed]));
        let c = commitment_from(&report);
        assert_eq!(c.transaction_uid, "9.9.9");
        assert_eq!(c.committed, vec!["1.2.3.4"]);
        assert_eq!(c.failed, vec![("1.2.3.5".to_string(), 0x0112)]);
        assert!(c.summary().contains("not 1"), "{}", c.summary());
    }

    #[test]
    fn a_worklist_query_asks_for_the_step_and_matches_part_of_a_name() {
        let q = worklist_identifier(&WorklistQuery {
            date: "20261107".into(),
            modality: "US".into(),
            patient: "bramble".into(),
            ..Default::default()
        });
        assert_eq!(str_of(&q, tags::PATIENT_NAME), "*bramble*");
        let step = q
            .element(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)
            .unwrap()
            .items()
            .unwrap()[0]
            .clone();
        assert_eq!(str_of(&step, tags::SCHEDULED_PROCEDURE_STEP_START_DATE), "20261107");
        assert_eq!(str_of(&step, tags::MODALITY), "US");
        let item = work_item(&q);
        assert_eq!((item.date.as_str(), item.modality.as_str()), ("20261107", "US"));
    }
}
