//! Ruthos's PACS client against a small PACS running in the test: echo,
//! query, C-GET retrieval, C-MOVE refusal, sending, and the plain-words
//! errors for a wrong AE title and a closed port.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
use dicom_ul::association::server::ServerAssociationOptions;
use ruthos_net::dimse::{
    self, command, context_for, decode, encode, receive_message, send_message, status_element, str_of, u16_of, Received,
};
use ruthos_net::{
    echo, find_studies, retrieve_study, send_files, NetError, Node, RetrieveMethod, RetrieveProgress, StudyQuery,
};

const STUDY: &str = "1.2.826.0.1.3680043.10.1465.99.1";
const SERIES_A: &str = "1.2.826.0.1.3680043.10.1465.99.2";
const SERIES_B: &str = "1.2.826.0.1.3680043.10.1465.99.5";

fn image(sop: &str) -> InMemDicomObject {
    let mut o = InMemDicomObject::new_empty();
    let s = |o: &mut InMemDicomObject, tag, vr, v: &str| o.put(DataElement::new(tag, vr, PrimitiveValue::from(v)));
    s(&mut o, tags::SOP_CLASS_UID, VR::UI, uids::CT_IMAGE_STORAGE);
    s(&mut o, tags::SOP_INSTANCE_UID, VR::UI, sop);
    s(&mut o, tags::STUDY_INSTANCE_UID, VR::UI, STUDY);
    s(
        &mut o,
        tags::SERIES_INSTANCE_UID,
        VR::UI,
        "1.2.826.0.1.3680043.10.1465.99.2",
    );
    s(&mut o, tags::PATIENT_NAME, VR::PN, "PONY^PERCY");
    s(&mut o, tags::PATIENT_ID, VR::LO, "P123");
    s(&mut o, tags::MODALITY, VR::CS, "CT");
    o.put(DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(2u16)));
    o.put(DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(2u16)));
    o.put(DataElement::new(
        tags::BITS_ALLOCATED,
        VR::US,
        PrimitiveValue::from(16u16),
    ));
    o.put(DataElement::new(tags::BITS_STORED, VR::US, PrimitiveValue::from(16u16)));
    o.put(DataElement::new(tags::HIGH_BIT, VR::US, PrimitiveValue::from(15u16)));
    o.put(DataElement::new(
        tags::SAMPLES_PER_PIXEL,
        VR::US,
        PrimitiveValue::from(1u16),
    ));
    o.put(DataElement::new(
        tags::PIXEL_REPRESENTATION,
        VR::US,
        PrimitiveValue::from(0u16),
    ));
    s(&mut o, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    o.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OW,
        PrimitiveValue::U16(vec![1, 2, 3, 4].into()),
    ));
    o
}

/// Start the test PACS (AE title TESTPACS) and return its port and a
/// counter of images it received.
fn start_pacs() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stored = Arc::new(AtomicUsize::new(0));
    let stored2 = stored.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let stored = stored2.clone();
            std::thread::spawn(move || {
                let mut options = ServerAssociationOptions::new()
                    .accept_called_ae_title()
                    .ae_title("TESTPACS")
                    .promiscuous(true);
                for s in [
                    uids::VERIFICATION,
                    uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_FIND,
                    uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_GET,
                    uids::STUDY_ROOT_QUERY_RETRIEVE_INFORMATION_MODEL_MOVE,
                    uids::CT_IMAGE_STORAGE,
                ] {
                    options = options.with_abstract_syntax(s);
                }
                let Ok(mut assoc) = options.establish(stream) else {
                    return;
                };
                serve(&mut assoc, &stored);
            });
        }
    });
    (port, stored)
}

fn serve(assoc: &mut dicom_ul::association::server::ServerAssociation<std::net::TcpStream>, stored: &AtomicUsize) {
    loop {
        let Ok(Received::Message(m)) = receive_message(assoc) else {
            return;
        };
        let id = u16_of(&m.command, tags::MESSAGE_ID);
        let sop_class = str_of(&m.command, tags::AFFECTED_SOP_CLASS_UID);
        let ts = dimse::transfer_syntax_of(assoc, m.pc_id).unwrap();
        match m.field() {
            dimse::C_ECHO_RQ => {
                let rsp = command(dimse::C_ECHO_RSP, id, &sop_class, false, vec![status_element(0)]);
                send_message(assoc, m.pc_id, &rsp, None).unwrap();
            }
            dimse::C_FIND_RQ => {
                let q = decode(m.data.as_deref().unwrap(), &ts).unwrap();
                if str_of(&q, tags::QUERY_RETRIEVE_LEVEL) == "SERIES" {
                    for (n, (uid, modality)) in [(SERIES_B, "SR"), (SERIES_A, "CT")].iter().enumerate() {
                        let mut r = InMemDicomObject::new_empty();
                        for (tag, vr, v) in [
                            (tags::QUERY_RETRIEVE_LEVEL, VR::CS, "SERIES"),
                            (tags::STUDY_INSTANCE_UID, VR::UI, STUDY),
                            (tags::SERIES_INSTANCE_UID, VR::UI, *uid),
                            (tags::MODALITY, VR::CS, *modality),
                            (tags::SERIES_NUMBER, VR::IS, if n == 0 { "2" } else { "1" }),
                            (tags::NUMBER_OF_SERIES_RELATED_INSTANCES, VR::IS, "2"),
                        ] {
                            r.put(DataElement::new(tag, vr, PrimitiveValue::from(v)));
                        }
                        let rsp = command(dimse::C_FIND_RSP, id, &sop_class, true, vec![status_element(0xFF00)]);
                        send_message(assoc, m.pc_id, &rsp, Some(&encode(&r, &ts).unwrap())).unwrap();
                    }
                    let done = command(dimse::C_FIND_RSP, id, &sop_class, false, vec![status_element(0)]);
                    send_message(assoc, m.pc_id, &done, None).unwrap();
                    continue;
                }
                let name = str_of(&q, tags::PATIENT_NAME);
                let pid = str_of(&q, tags::PATIENT_ID);
                // One study for PONY or P123, none otherwise.
                if name.to_uppercase().contains("PONY") || pid == "P123" {
                    let mut r = InMemDicomObject::new_empty();
                    for (tag, vr, v) in [
                        (tags::QUERY_RETRIEVE_LEVEL, VR::CS, "STUDY"),
                        (tags::STUDY_INSTANCE_UID, VR::UI, STUDY),
                        (tags::PATIENT_NAME, VR::PN, "PONY^PERCY"),
                        (tags::PATIENT_ID, VR::LO, "P123"),
                        (tags::STUDY_DATE, VR::DA, "20261101"),
                        (tags::MODALITIES_IN_STUDY, VR::CS, "CT"),
                        (tags::NUMBER_OF_STUDY_RELATED_INSTANCES, VR::IS, "2"),
                    ] {
                        r.put(DataElement::new(tag, vr, PrimitiveValue::from(v)));
                    }
                    let rsp = command(dimse::C_FIND_RSP, id, &sop_class, true, vec![status_element(0xFF00)]);
                    send_message(assoc, m.pc_id, &rsp, Some(&encode(&r, &ts).unwrap())).unwrap();
                }
                let done = command(dimse::C_FIND_RSP, id, &sop_class, false, vec![status_element(0)]);
                send_message(assoc, m.pc_id, &done, None).unwrap();
            }
            dimse::C_GET_RQ => {
                let q = decode(m.data.as_deref().unwrap(), &ts).unwrap();
                let series_only = str_of(&q, tags::QUERY_RETRIEVE_LEVEL) == "SERIES";
                assert!(!series_only || str_of(&q, tags::SERIES_INSTANCE_UID) == SERIES_A);
                let (store_pc, store_ts) = context_for(assoc, uids::CT_IMAGE_STORAGE).unwrap();
                let sops: &[&str] = if series_only {
                    &["1.2.826.0.1.3680043.10.1465.99.3"]
                } else {
                    &["1.2.826.0.1.3680043.10.1465.99.3", "1.2.826.0.1.3680043.10.1465.99.4"]
                };
                let last = sops.len() as u16 - 1;
                for (n, sop) in sops.iter().enumerate() {
                    let rq = command(
                        dimse::C_STORE_RQ,
                        100 + n as u16,
                        uids::CT_IMAGE_STORAGE,
                        true,
                        vec![DataElement::new(
                            tags::AFFECTED_SOP_INSTANCE_UID,
                            VR::UI,
                            PrimitiveValue::from(*sop),
                        )],
                    );
                    send_message(assoc, store_pc, &rq, Some(&encode(&image(sop), &store_ts).unwrap())).unwrap();
                    let Ok(Received::Message(rsp)) = receive_message(assoc) else {
                        return;
                    };
                    assert_eq!(rsp.status(), 0);
                    let pending = command(
                        dimse::C_GET_RSP,
                        id,
                        &sop_class,
                        false,
                        vec![
                            status_element(0xFF00),
                            DataElement::new(
                                tags::NUMBER_OF_REMAINING_SUBOPERATIONS,
                                VR::US,
                                PrimitiveValue::from(last - n as u16),
                            ),
                            DataElement::new(
                                tags::NUMBER_OF_COMPLETED_SUBOPERATIONS,
                                VR::US,
                                PrimitiveValue::from(n as u16 + 1),
                            ),
                            DataElement::new(tags::NUMBER_OF_FAILED_SUBOPERATIONS, VR::US, PrimitiveValue::from(0u16)),
                            DataElement::new(
                                tags::NUMBER_OF_WARNING_SUBOPERATIONS,
                                VR::US,
                                PrimitiveValue::from(0u16),
                            ),
                        ],
                    );
                    send_message(assoc, m.pc_id, &pending, None).unwrap();
                }
                let done = command(dimse::C_GET_RSP, id, &sop_class, false, vec![status_element(0)]);
                send_message(assoc, m.pc_id, &done, None).unwrap();
            }
            dimse::C_MOVE_RQ => {
                // Ruthos is not configured on this PACS.
                let rsp = command(dimse::C_MOVE_RSP, id, &sop_class, false, vec![status_element(0xA801)]);
                send_message(assoc, m.pc_id, &rsp, None).unwrap();
            }
            dimse::C_STORE_RQ => {
                let obj = decode(m.data.as_deref().unwrap(), &ts).unwrap();
                assert_eq!(str_of(&obj, tags::PATIENT_ID), "P123");
                stored.fetch_add(1, Ordering::SeqCst);
                let sop = str_of(&m.command, tags::AFFECTED_SOP_INSTANCE_UID);
                let rsp = command(
                    dimse::C_STORE_RSP,
                    id,
                    &sop_class,
                    false,
                    vec![
                        status_element(0),
                        DataElement::new(
                            tags::AFFECTED_SOP_INSTANCE_UID,
                            VR::UI,
                            PrimitiveValue::from(sop.as_str()),
                        ),
                    ],
                );
                send_message(assoc, m.pc_id, &rsp, None).unwrap();
            }
            _ => return,
        }
    }
}

fn node(port: u16) -> Node {
    Node {
        host: "127.0.0.1".into(),
        port,
        ae_title: "TESTPACS".into(),
    }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruthos-pacs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn echo_answers_and_a_wrong_ae_title_is_named() {
    let (port, _) = start_pacs();
    assert!(echo(&node(port), "RUTHOS").is_ok());
    let wrong = Node {
        ae_title: "NOTPACS".into(),
        ..node(port)
    };
    assert_eq!(echo(&wrong, "RUTHOS").unwrap_err(), NetError::CalledAeRejected);
}

#[test]
fn a_search_by_name_or_id_finds_the_study() {
    let (port, _) = start_pacs();
    let never = AtomicBool::new(false);
    for text in ["pony", "P123"] {
        let q = StudyQuery {
            patient: text.into(),
            ..Default::default()
        };
        let r = find_studies(&node(port), "RUTHOS", &q, &never).unwrap();
        assert_eq!(r.len(), 1, "search for {text}");
        assert_eq!(r[0].study_uid, STUDY);
        assert_eq!(r[0].instances, Some(2));
        assert_eq!(r[0].study_date, "20261101");
    }
    let none = StudyQuery {
        patient: "zebra".into(),
        ..Default::default()
    };
    assert!(find_studies(&node(port), "RUTHOS", &none, &never).unwrap().is_empty());
}

#[test]
fn c_get_brings_the_images_into_the_folder() {
    let (port, _) = start_pacs();
    let out = temp("get");
    let never = AtomicBool::new(false);
    let seen = std::sync::Mutex::new(Vec::<RetrieveProgress>::new());
    let r = retrieve_study(
        &node(port),
        "RUTHOS",
        STUDY,
        RetrieveMethod::Auto,
        &out,
        &|p| seen.lock().unwrap().push(p),
        &never,
    )
    .unwrap();
    assert_eq!(r.method, RetrieveMethod::Get);
    assert_eq!(r.files.len(), 2);
    assert_eq!(r.progress.completed, 2);
    for f in &r.files {
        let obj = dicom_object::open_file(f).unwrap();
        assert_eq!(str_of(&obj, tags::STUDY_INSTANCE_UID), STUDY);
    }
    assert!(seen.lock().unwrap().iter().any(|p| p.total() == Some(2)));
}

#[test]
fn a_study_lists_its_series_and_one_series_can_be_retrieved() {
    let (port, _) = start_pacs();
    let never = AtomicBool::new(false);
    let series = ruthos_net::find_series(&node(port), "RUTHOS", STUDY, &never).unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(
        (series[0].number, series[0].modality.as_str()),
        (Some(1), "CT"),
        "in series order"
    );
    assert_eq!(series[0].instances, Some(2));
    let out = temp("series");
    let r = ruthos_net::retrieve_series(
        &node(port),
        "RUTHOS",
        STUDY,
        SERIES_A,
        RetrieveMethod::Get,
        &out,
        &|_| {},
        &never,
    )
    .unwrap();
    assert_eq!(r.files.len(), 1, "only that series");
}

#[test]
fn c_move_to_an_unknown_destination_is_explained() {
    let (port, _) = start_pacs();
    let never = AtomicBool::new(false);
    let r = retrieve_study(
        &node(port),
        "RUTHOS",
        STUDY,
        RetrieveMethod::Move,
        &temp("move"),
        &|_| {},
        &never,
    );
    assert_eq!(r.unwrap_err(), NetError::MoveDestinationUnknown);
}

#[test]
fn sending_files_stores_them_on_the_pacs() {
    let (port, stored) = start_pacs();
    let dir = temp("send");
    let mut files = Vec::new();
    for sop in ["1.2.826.0.1.3680043.10.1465.99.7", "1.2.826.0.1.3680043.10.1465.99.8"] {
        let meta = FileMetaTableBuilder::new()
            .media_storage_sop_class_uid(uids::CT_IMAGE_STORAGE)
            .media_storage_sop_instance_uid(sop)
            .transfer_syntax(dimse::EXPLICIT_VR_LE)
            .build()
            .unwrap();
        let path = dir.join(format!("{sop}.dcm"));
        image(sop).with_exact_meta(meta).write_to_file(&path).unwrap();
        files.push(path);
    }
    files.push(dir.join("missing.dcm"));
    let never = AtomicBool::new(false);
    let done = AtomicUsize::new(0);
    let r = send_files(
        &node(port),
        "RUTHOS",
        &files,
        &|n, _| done.store(n, Ordering::SeqCst),
        &never,
    )
    .unwrap();
    assert_eq!(r.sent, 2);
    assert_eq!(r.failed.len(), 1, "the missing file is reported, not fatal");
    assert_eq!(stored.load(Ordering::SeqCst), 2);
    assert_eq!(done.load(Ordering::SeqCst), 2);
}
