//! C-ECHO client, for testing a PACS node from the preferences window.

use std::time::{Duration, Instant};

use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{InMemDicomObject, StandardDataDictionary};
use dicom_ul::association::client::ClientAssociationOptions;
use dicom_ul::pdu::{PDataValue, PDataValueType};
use dicom_ul::Pdu;

/// Send a C-ECHO to `host:port` and return the round trip time.
pub fn echo(host: &str, port: u16, called_ae: &str, calling_ae: &str, timeout: Duration) -> anyhow::Result<Duration> {
    let start = Instant::now();
    let addr = format!("{host}:{port}");
    let mut association = ClientAssociationOptions::new()
        .with_abstract_syntax(uids::VERIFICATION)
        .calling_ae_title(calling_ae)
        .called_ae_title(called_ae)
        .connection_timeout(timeout)
        .read_timeout(timeout)
        .write_timeout(timeout)
        .establish_with(&addr)?;

    let pc = association
        .presentation_contexts()
        .first()
        .ok_or_else(|| anyhow::anyhow!("no presentation context accepted"))?
        .clone();

    let ts = dicom_transfer_syntax_registry::entries::IMPLICIT_VR_LITTLE_ENDIAN.erased();
    let message_id: u16 = 1;
    let cmd = create_echo_command(message_id);
    let mut data = Vec::new();
    cmd.write_dataset_with_ts(&mut data, &ts)?;
    association.send(&Pdu::PData {
        data: vec![PDataValue {
            presentation_context_id: pc.id,
            value_type: PDataValueType::Command,
            is_last: true,
            data,
        }],
    })?;

    let pdu = association.receive()?;
    let result = match pdu {
        Pdu::PData { data } => {
            let first = data.first().ok_or_else(|| anyhow::anyhow!("empty response"))?;
            let obj = InMemDicomObject::read_dataset_with_ts(first.data.as_slice(), &ts)?;
            let status: u16 = obj.element(tags::STATUS)?.to_int()?;
            if status == 0 {
                Ok(start.elapsed())
            } else {
                Err(anyhow::anyhow!("C-ECHO returned status {status:04X}H"))
            }
        }
        other => Err(anyhow::anyhow!("unexpected PDU {other:?}")),
    };
    let _ = association.release();
    result
}

fn create_echo_command(message_id: u16) -> InMemDicomObject<StandardDataDictionary> {
    InMemDicomObject::command_from_element_iter([
        DataElement::new(
            tags::AFFECTED_SOP_CLASS_UID,
            VR::UI,
            dicom_value!(Str, uids::VERIFICATION),
        ),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [0x0030])),
        DataElement::new(tags::MESSAGE_ID, VR::US, dicom_value!(U16, [message_id])),
        DataElement::new(tags::COMMAND_DATA_SET_TYPE, VR::US, dicom_value!(U16, [0x0101])),
    ])
}
