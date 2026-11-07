//! Modality Worklist and Storage Commitment against a small server in the
//! test: a worklist search, a commitment answered on the same association with
//! one image not held, and a commitment answered later to the listener.

use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dicom_core::header::Length;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{dicom_value, DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_ul::association::client::ClientAssociationOptions;
use dicom_ul::association::server::ServerAssociationOptions;
use ruthos_net::dimse::{
    self, command, decode, encode, receive_message, send_message, status_element, str_of, Received,
};
use ruthos_net::scp::AllowEntry;
use ruthos_net::workflow::{
    commitment_request, COMMITMENT_INSTANCE, N_ACTION_RQ, N_ACTION_RSP, N_EVENT_REPORT_RQ, N_EVENT_REPORT_RSP,
    STORAGE_COMMITMENT, WORKLIST_FIND,
};
use ruthos_net::{
    find_worklist, request_commitment, CommitOutcome, ListenerConfig, NetEvent, Node, StoreScp, WorklistQuery,
};

const CLASS: &str = "1.2.840.10008.5.1.4.1.1.6.1";

fn el(tag: dicom_core::Tag, vr: VR, v: &str) -> dicom_object::mem::InMemElement {
    DataElement::new(tag, vr, PrimitiveValue::from(v))
}

fn seq(tag: dicom_core::Tag, items: Vec<InMemDicomObject>) -> dicom_object::mem::InMemElement {
    DataElement::new(
        tag,
        VR::SQ,
        Value::Sequence(DataSetSequence::new(items, Length::UNDEFINED)),
    )
}

/// The report a PACS makes: every image held, except any named `...999`.
fn report(request: &InMemDicomObject) -> InMemDicomObject {
    let items = request
        .element(tags::REFERENCED_SOP_SEQUENCE)
        .unwrap()
        .items()
        .unwrap()
        .to_vec();
    let (bad, good): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|i| str_of(i, tags::REFERENCED_SOP_INSTANCE_UID).ends_with(".999"));
    let mut out = InMemDicomObject::new_empty();
    out.put(el(
        tags::TRANSACTION_UID,
        VR::UI,
        &str_of(request, tags::TRANSACTION_UID),
    ));
    out.put(seq(tags::REFERENCED_SOP_SEQUENCE, good));
    if !bad.is_empty() {
        let bad = bad
            .into_iter()
            .map(|mut i| {
                i.put(DataElement::new(
                    tags::FAILURE_REASON,
                    VR::US,
                    dicom_value!(U16, [0x0112]),
                ));
                i
            })
            .collect();
        out.put(seq(tags::FAILED_SOP_SEQUENCE, bad));
    }
    out
}

fn event_report(id: u16, failures: bool) -> InMemDicomObject {
    InMemDicomObject::command_from_element_iter([
        el(tags::AFFECTED_SOP_CLASS_UID, VR::UI, STORAGE_COMMITMENT),
        DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [N_EVENT_REPORT_RQ])),
        DataElement::new(tags::MESSAGE_ID, VR::US, dicom_value!(U16, [id])),
        DataElement::new(
            tags::COMMAND_DATA_SET_TYPE,
            VR::US,
            dicom_value!(U16, [dimse::HAS_DATA_SET]),
        ),
        el(tags::AFFECTED_SOP_INSTANCE_UID, VR::UI, COMMITMENT_INSTANCE),
        DataElement::new(
            tags::EVENT_TYPE_ID,
            VR::US,
            dicom_value!(U16, [if failures { 2 } else { 1 }]),
        ),
    ])
}

/// A worklist and commitment server, AE title TESTRIS.
fn start_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let options = ServerAssociationOptions::new()
                    .accept_called_ae_title()
                    .ae_title("TESTRIS")
                    .with_abstract_syntax(WORKLIST_FIND)
                    .with_abstract_syntax(STORAGE_COMMITMENT);
                let Ok(mut assoc) = options.establish(stream) else {
                    return;
                };
                loop {
                    let Ok(Received::Message(m)) = receive_message(&mut assoc) else {
                        return;
                    };
                    let ts = dimse::transfer_syntax_of(&assoc, m.pc_id).unwrap();
                    let id = dimse::u16_of(&m.command, tags::MESSAGE_ID);
                    match m.field() {
                        dimse::C_FIND_RQ => {
                            let q = decode(m.data.as_deref().unwrap(), &ts).unwrap();
                            assert_eq!(str_of(&q, tags::PATIENT_NAME), "*bramble*");
                            let mut step = InMemDicomObject::new_empty();
                            step.put(el(tags::MODALITY, VR::CS, "US"));
                            step.put(el(tags::SCHEDULED_PROCEDURE_STEP_START_DATE, VR::DA, "20261107"));
                            step.put(el(tags::SCHEDULED_PROCEDURE_STEP_START_TIME, VR::TM, "0930"));
                            step.put(el(tags::SCHEDULED_STATION_AE_TITLE, VR::AE, "ECHO1"));
                            let mut item = InMemDicomObject::new_empty();
                            item.put(el(tags::PATIENT_NAME, VR::PN, "BRAMBLE^SYNTHETIC"));
                            item.put(el(tags::PATIENT_ID, VR::LO, "V77"));
                            item.put(el(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, "Canine"));
                            item.put(el(tags::ACCESSION_NUMBER, VR::SH, "ACC42"));
                            item.put(el(tags::STUDY_INSTANCE_UID, VR::UI, "1.2.826.0.1.3680043.10.1465.77"));
                            item.put(el(tags::REQUESTED_PROCEDURE_DESCRIPTION, VR::LO, "Echocardiogram"));
                            item.put(seq(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE, vec![step]));
                            let rsp = command(dimse::C_FIND_RSP, id, WORKLIST_FIND, true, vec![status_element(0xFF00)]);
                            send_message(&mut assoc, m.pc_id, &rsp, Some(&encode(&item, &ts).unwrap())).unwrap();
                            let done = command(dimse::C_FIND_RSP, id, WORKLIST_FIND, false, vec![status_element(0)]);
                            send_message(&mut assoc, m.pc_id, &done, None).unwrap();
                        }
                        N_ACTION_RQ => {
                            assert_eq!(
                                str_of(&m.command, tags::REQUESTED_SOP_INSTANCE_UID),
                                COMMITMENT_INSTANCE
                            );
                            let request = decode(m.data.as_deref().unwrap(), &ts).unwrap();
                            let rsp = InMemDicomObject::command_from_element_iter([
                                el(tags::AFFECTED_SOP_CLASS_UID, VR::UI, STORAGE_COMMITMENT),
                                DataElement::new(tags::COMMAND_FIELD, VR::US, dicom_value!(U16, [N_ACTION_RSP])),
                                DataElement::new(tags::MESSAGE_ID_BEING_RESPONDED_TO, VR::US, dicom_value!(U16, [id])),
                                DataElement::new(
                                    tags::COMMAND_DATA_SET_TYPE,
                                    VR::US,
                                    dicom_value!(U16, [dimse::NO_DATA_SET]),
                                ),
                                status_element(0),
                            ]);
                            send_message(&mut assoc, m.pc_id, &rsp, None).unwrap();
                            let answer = report(&request);
                            let failures = answer.element(tags::FAILED_SOP_SEQUENCE).is_ok();
                            send_message(
                                &mut assoc,
                                m.pc_id,
                                &event_report(7, failures),
                                Some(&encode(&answer, &ts).unwrap()),
                            )
                            .unwrap();
                        }
                        N_EVENT_REPORT_RSP => {}
                        _ => return,
                    }
                }
            });
        }
    });
    port
}

fn node(port: u16, ae: &str) -> Node {
    Node {
        host: "127.0.0.1".into(),
        port,
        ae_title: ae.into(),
    }
}

#[test]
fn a_worklist_search_reads_the_scheduled_step() {
    let port = start_server();
    let q = WorklistQuery {
        patient: "bramble".into(),
        ..Default::default()
    };
    let items = find_worklist(&node(port, "TESTRIS"), "RUTHOS", &q, &AtomicBool::new(false)).unwrap();
    assert_eq!(items.len(), 1);
    let w = &items[0];
    assert_eq!((w.patient_id.as_str(), w.accession.as_str()), ("V77", "ACC42"));
    assert_eq!((w.modality.as_str(), w.station.as_str()), ("US", "ECHO1"));
    assert_eq!((w.date.as_str(), w.time.as_str()), ("20261107", "0930"));
    assert_eq!(w.species, "Canine");
    assert_eq!(w.procedure, "Echocardiogram");
}

#[test]
fn a_commitment_on_the_same_association_names_what_is_not_held() {
    let port = start_server();
    let items = vec![
        (CLASS.to_string(), "1.2.826.0.1.3680043.10.1465.1".to_string()),
        (CLASS.to_string(), "1.2.826.0.1.3680043.10.1465.999".to_string()),
    ];
    let outcome = request_commitment(
        &node(port, "TESTRIS"),
        "RUTHOS",
        "1.2.826.0.1.3680043.10.1465.55",
        &items,
        Duration::from_secs(5),
    )
    .unwrap();
    let CommitOutcome::Answered(c) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(c.transaction_uid, "1.2.826.0.1.3680043.10.1465.55");
    assert_eq!(c.committed, vec!["1.2.826.0.1.3680043.10.1465.1"]);
    assert_eq!(c.failed, vec![("1.2.826.0.1.3680043.10.1465.999".to_string(), 0x0112)]);
}

#[test]
fn a_commitment_answered_later_reaches_the_listener() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let allow = Arc::new(Mutex::new(vec![AllowEntry {
        name: "PACS".into(),
        ae_title: "TESTPACS".into(),
        address: String::new(),
    }]));
    let mut scp = StoreScp::start(ListenerConfig {
        port,
        ae_title: "RUTHOS".into(),
        out_dir: std::env::temp_dir().join(format!("ruthos-commit-{}", std::process::id())),
        allow,
        promiscuous: false,
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // The PACS calls back with its report.
    let mut assoc = ClientAssociationOptions::new()
        .calling_ae_title("TESTPACS")
        .called_ae_title("RUTHOS")
        .with_presentation_context(STORAGE_COMMITMENT, vec![dimse::EXPLICIT_VR_LE])
        .establish_with(&format!("127.0.0.1:{port}"))
        .unwrap();
    let (pc, ts) = dimse::context_for(&assoc, STORAGE_COMMITMENT).expect("the listener takes reports");
    let items = vec![(CLASS.to_string(), "1.2.826.0.1.3680043.10.1465.3".to_string())];
    let answer = report(&commitment_request("1.2.826.0.1.3680043.10.1465.66", &items));
    send_message(
        &mut assoc,
        pc,
        &event_report(3, false),
        Some(&encode(&answer, &ts).unwrap()),
    )
    .unwrap();
    let Received::Message(rsp) = receive_message(&mut assoc).unwrap() else {
        panic!("no reply")
    };
    assert_eq!(rsp.field(), N_EVENT_REPORT_RSP);
    assert_eq!(rsp.status(), 0);
    assert_eq!(dimse::u16_of(&rsp.command, tags::MESSAGE_ID_BEING_RESPONDED_TO), 3);
    let _ = assoc.release();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut seen = None;
    while seen.is_none() && std::time::Instant::now() < deadline {
        while let Ok(ev) = scp.events.try_recv() {
            if let NetEvent::Committed { peer_ae, commitment } = ev {
                seen = Some((peer_ae, commitment));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (peer, c) = seen.expect("the listener reports the commitment");
    assert_eq!(peer, "TESTPACS");
    assert_eq!(c.transaction_uid, "1.2.826.0.1.3680043.10.1465.66");
    assert_eq!(c.committed.len(), 1);
    assert!(c.failed.is_empty());
    scp.stop();
}
