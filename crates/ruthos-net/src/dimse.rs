//! DIMSE messages over an association: commands, data sets, fragments.
//!
//! The same code drives Ruthos as a client (query, retrieve, send) and the
//! test PACS in `tests/`. A message is a command set, encoded in Implicit
//! VR Little Endian as the standard requires, optionally followed by a data
//! set in the presentation context's transfer syntax; either may span
//! several P-DATA fragments.

use std::net::TcpStream;

use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::tags;
use dicom_encoding::transfer_syntax::TransferSyntaxIndex;
use dicom_object::{InMemDicomObject, StandardDataDictionary};
use dicom_transfer_syntax_registry::TransferSyntaxRegistry;
use dicom_ul::association::client::ClientAssociation;
use dicom_ul::association::server::ServerAssociation;
use dicom_ul::pdu::{PDataValue, PDataValueType, PresentationContextResultReason};
use dicom_ul::Pdu;

use crate::error::NetError;

pub type Obj = InMemDicomObject<StandardDataDictionary>;

pub const C_STORE_RQ: u16 = 0x0001;
pub const C_STORE_RSP: u16 = 0x8001;
pub const C_GET_RQ: u16 = 0x0010;
pub const C_GET_RSP: u16 = 0x8010;
pub const C_FIND_RQ: u16 = 0x0020;
pub const C_FIND_RSP: u16 = 0x8020;
pub const C_MOVE_RQ: u16 = 0x0021;
pub const C_MOVE_RSP: u16 = 0x8021;
pub const C_ECHO_RQ: u16 = 0x0030;
pub const C_ECHO_RSP: u16 = 0x8030;
pub const C_CANCEL_RQ: u16 = 0x0FFF;

/// Command Data Set Type: no data set follows.
pub const NO_DATA_SET: u16 = 0x0101;
/// Command Data Set Type: a data set follows (any value but 0101H).
pub const HAS_DATA_SET: u16 = 0x0000;

pub const IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";
pub const EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1";

/// Status classes (PS3.7 C).
pub fn is_pending(status: u16) -> bool {
    status == 0xFF00 || status == 0xFF01
}

pub fn is_success(status: u16) -> bool {
    status == 0x0000
}

pub fn is_warning(status: u16) -> bool {
    status == 0x0001 || (0xB000..=0xBFFF).contains(&status)
}

pub fn is_cancel(status: u16) -> bool {
    status == 0xFE00
}

/// Both sides of an association look the same to the message code.
pub trait Link {
    fn send_pdu(&mut self, pdu: &Pdu) -> Result<(), NetError>;
    fn receive_pdu(&mut self) -> Result<Pdu, NetError>;
    /// Largest PDU the peer accepts.
    fn peer_max_pdu(&self) -> u32;
    fn contexts(&self) -> Vec<(u8, String, String)>;
}

fn accepted(pcs: &[dicom_ul::pdu::PresentationContextNegotiated]) -> Vec<(u8, String, String)> {
    pcs.iter()
        .filter(|pc| pc.reason == PresentationContextResultReason::Acceptance)
        .map(|pc| {
            (
                pc.id,
                pc.abstract_syntax.trim_end_matches('\0').to_string(),
                pc.transfer_syntax.trim_end_matches('\0').to_string(),
            )
        })
        .collect()
}

impl Link for ClientAssociation<TcpStream> {
    fn send_pdu(&mut self, pdu: &Pdu) -> Result<(), NetError> {
        self.send(pdu).map_err(NetError::from_ul)
    }
    fn receive_pdu(&mut self) -> Result<Pdu, NetError> {
        self.receive().map_err(NetError::from_ul)
    }
    fn peer_max_pdu(&self) -> u32 {
        self.acceptor_max_pdu_length()
    }
    fn contexts(&self) -> Vec<(u8, String, String)> {
        accepted(self.presentation_contexts())
    }
}

impl Link for ServerAssociation<TcpStream> {
    fn send_pdu(&mut self, pdu: &Pdu) -> Result<(), NetError> {
        self.send(pdu).map_err(NetError::from_ul)
    }
    fn receive_pdu(&mut self) -> Result<Pdu, NetError> {
        self.receive().map_err(NetError::from_ul)
    }
    fn peer_max_pdu(&self) -> u32 {
        self.requestor_max_pdu_length()
    }
    fn contexts(&self) -> Vec<(u8, String, String)> {
        accepted(self.presentation_contexts())
    }
}

/// The accepted presentation context for an abstract syntax, with its
/// transfer syntax.
pub fn context_for(link: &dyn Link, abstract_syntax: &str) -> Option<(u8, String)> {
    link.contexts()
        .into_iter()
        .find(|(_, a, _)| a == abstract_syntax)
        .map(|(id, _, ts)| (id, ts))
}

/// The transfer syntax of a presentation context.
pub fn transfer_syntax_of(link: &dyn Link, pc_id: u8) -> Option<String> {
    link.contexts()
        .into_iter()
        .find(|(id, _, _)| *id == pc_id)
        .map(|(_, _, ts)| ts)
}

pub fn encode(obj: &Obj, ts_uid: &str) -> Result<Vec<u8>, NetError> {
    let ts = TransferSyntaxRegistry
        .get(ts_uid)
        .ok_or_else(|| NetError::Other(format!("unknown transfer syntax {ts_uid}")))?;
    let mut out = Vec::new();
    obj.write_dataset_with_ts(&mut out, ts)
        .map_err(|e| NetError::Other(format!("could not encode: {e}")))?;
    Ok(out)
}

pub fn decode(bytes: &[u8], ts_uid: &str) -> Result<Obj, NetError> {
    let ts = TransferSyntaxRegistry
        .get(ts_uid)
        .ok_or_else(|| NetError::Other(format!("unknown transfer syntax {ts_uid}")))?;
    InMemDicomObject::read_dataset_with_ts(bytes, ts).map_err(|e| NetError::Other(format!("could not decode: {e}")))
}

/// Send one message: the command, then the data set when there is one,
/// each split to fit the peer's maximum PDU.
pub fn send_message(link: &mut dyn Link, pc_id: u8, command: &Obj, data: Option<&[u8]>) -> Result<(), NetError> {
    let cmd = encode(command, IMPLICIT_VR_LE)?;
    send_fragments(link, pc_id, PDataValueType::Command, &cmd)?;
    if let Some(d) = data {
        send_fragments(link, pc_id, PDataValueType::Data, d)?;
    }
    Ok(())
}

fn send_fragments(link: &mut dyn Link, pc_id: u8, kind: PDataValueType, bytes: &[u8]) -> Result<(), NetError> {
    // PDU header 6 bytes, PDV item header 6 bytes.
    let max = link.peer_max_pdu();
    let chunk = if max == 0 {
        16_384
    } else {
        (max as usize).saturating_sub(12).max(1024)
    };
    let pieces: Vec<&[u8]> = if bytes.is_empty() {
        vec![bytes]
    } else {
        bytes.chunks(chunk).collect()
    };
    let n = pieces.len();
    for (i, piece) in pieces.into_iter().enumerate() {
        link.send_pdu(&Pdu::PData {
            data: vec![PDataValue {
                presentation_context_id: pc_id,
                value_type: kind.clone(),
                is_last: i + 1 == n,
                data: piece.to_vec(),
            }],
        })?;
    }
    Ok(())
}

/// One received message.
pub struct Message {
    pub pc_id: u8,
    pub command: Obj,
    /// The data set's bytes in the context's transfer syntax.
    pub data: Option<Vec<u8>>,
}

impl Message {
    pub fn field(&self) -> u16 {
        u16_of(&self.command, tags::COMMAND_FIELD)
    }

    pub fn status(&self) -> u16 {
        u16_of(&self.command, tags::STATUS)
    }
}

pub fn u16_of(obj: &Obj, tag: dicom_core::Tag) -> u16 {
    obj.element(tag).ok().and_then(|e| e.to_int::<u16>().ok()).unwrap_or(0)
}

pub fn str_of(obj: &Obj, tag: dicom_core::Tag) -> String {
    obj.element(tag)
        .ok()
        .and_then(|e| {
            e.to_str()
                .ok()
                .map(|s| s.trim_end_matches(['\0', ' ']).trim().to_string())
        })
        .unwrap_or_default()
}

/// What `receive_message` can return besides a message.
pub enum Received {
    Message(Message),
    /// The peer asked to release the association.
    Release,
}

/// Read PDUs until one whole message (command and data set) has arrived.
pub fn receive_message(link: &mut dyn Link) -> Result<Received, NetError> {
    let mut command_bytes: Vec<u8> = Vec::new();
    let mut command: Option<(u8, Obj)> = None;
    let mut data: Vec<u8> = Vec::new();
    loop {
        match link.receive_pdu()? {
            Pdu::PData { data: values } => {
                for v in values {
                    match v.value_type {
                        PDataValueType::Command => {
                            command_bytes.extend_from_slice(&v.data);
                            if v.is_last {
                                let cmd = decode(&command_bytes, IMPLICIT_VR_LE)?;
                                let has_data = u16_of(&cmd, tags::COMMAND_DATA_SET_TYPE) != NO_DATA_SET;
                                if !has_data {
                                    return Ok(Received::Message(Message {
                                        pc_id: v.presentation_context_id,
                                        command: cmd,
                                        data: None,
                                    }));
                                }
                                command = Some((v.presentation_context_id, cmd));
                            }
                        }
                        PDataValueType::Data => {
                            data.extend_from_slice(&v.data);
                            if v.is_last {
                                let Some((pc_id, cmd)) = command.take() else {
                                    return Err(NetError::Other("data set before its command".into()));
                                };
                                return Ok(Received::Message(Message {
                                    pc_id,
                                    command: cmd,
                                    data: Some(std::mem::take(&mut data)),
                                }));
                            }
                        }
                    }
                }
            }
            Pdu::ReleaseRQ => {
                let _ = link.send_pdu(&Pdu::ReleaseRP);
                return Ok(Received::Release);
            }
            Pdu::AbortRQ { .. } => return Err(NetError::Aborted),
            _ => {}
        }
    }
}

/// A command set from elements, with the data set type filled in.
pub fn command(
    field: u16,
    message_id: u16,
    sop_class: &str,
    has_data: bool,
    extra: Vec<dicom_object::mem::InMemElement>,
) -> Obj {
    let mut elements = vec![
        DataElement::new(tags::AFFECTED_SOP_CLASS_UID, VR::UI, dicom_value!(Str, sop_class)),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [field])),
        DataElement::new(
            if field & 0x8000 != 0 {
                tags::MESSAGE_ID_BEING_RESPONDED_TO
            } else {
                tags::MESSAGE_ID
            },
            VR::US,
            dicom_value!(U16, [message_id]),
        ),
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [if has_data { HAS_DATA_SET } else { NO_DATA_SET }]),
        ),
    ];
    elements.extend(extra);
    InMemDicomObject::command_from_element_iter(elements)
}

/// Priority (0000,0700): medium.
pub fn priority() -> dicom_object::mem::InMemElement {
    DataElement::new(tags::PRIORITY, VR::US, dicom_value!(U16, [0x0000]))
}

pub fn status_element(status: u16) -> dicom_object::mem::InMemElement {
    DataElement::new(tags::STATUS, VR::US, dicom_value!(U16, [status]))
}
