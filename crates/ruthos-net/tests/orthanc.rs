//! Against a real Orthanc with its DICOMweb plugin. `RUTHOS_ORTHANC_WEB` is the
//! service root (`http://localhost:8042/dicom-web`) and `RUTHOS_ORTHANC_DIMSE` is
//! `host:port:AE`. `RUTHOS_ORTHANC_SEG` is a SEG Ruthos wrote. CI runs an Orthanc;
//! elsewhere these tests pass without one.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use dicom_dictionary_std::tags;
use ruthos_net::dicomweb::{Basic, Transfer, WebClient, WebNode};
use ruthos_net::{find_studies, retrieve_series, send_files, Node, RetrieveMethod, RetrieveProgress, StudyQuery};

fn web() -> Option<WebClient> {
    let url = std::env::var("RUTHOS_ORTHANC_WEB").ok()?;
    let client = WebClient::new(WebNode {
        name: "Orthanc".into(),
        qido: url.clone(),
        wado: url.clone(),
        stow: url,
        sign_in: Arc::new(Basic {
            user: "orthanc".into(),
            password: "orthanc".into(),
        }),
        pin: None,
        client_certificate: None,
        proxy: None,
        transfer: Transfer::Original,
        page_size: 100,
        include_all: None,
        parallel: 4,
        most_parts: ruthos_net::dicomweb::multipart::MOST_PARTS,
    })
    .unwrap();
    Some(client)
}

fn dimse() -> Option<Node> {
    let text = std::env::var("RUTHOS_ORTHANC_DIMSE").ok()?;
    let mut parts = text.split(':');
    Some(Node {
        host: parts.next()?.to_string(),
        port: parts.next()?.parse().ok()?,
        ae_title: parts.next()?.to_string(),
    })
}

fn samples() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom");
    ["CT_small.dcm", "MR_small.dcm"].map(|n| dir.join(n)).to_vec()
}

fn sop_uids(files: &[PathBuf]) -> Vec<String> {
    let mut uids: Vec<String> = files
        .iter()
        .map(|f| ruthos_dicom::read_meta(f).unwrap().sop_instance_uid)
        .collect();
    uids.sort();
    uids
}

#[test]
fn send_search_and_retrieve_agree_with_classic_dicom() {
    let (Some(client), Some(node)) = (web(), dimse()) else {
        eprintln!("no Orthanc set up: skipped");
        return;
    };
    let never = AtomicBool::new(false);

    let sent = client.send_files(&samples(), &|_, _| {}, &never).unwrap();
    assert_eq!(sent.sent, 2, "{:?}", sent.failed);

    // The same search over both routes finds the same studies.
    for q in [
        StudyQuery::default(),
        StudyQuery {
            patient: "CompressedSamples".into(),
            ..StudyQuery::default()
        },
        StudyQuery {
            modality: "CT".into(),
            ..StudyQuery::default()
        },
        StudyQuery {
            study_date: "20040101-20041231".into(),
            ..StudyQuery::default()
        },
    ] {
        let by_web = client.find_studies(&q, &never).unwrap().studies;
        let by_dimse = find_studies(&node, "RUTHOS", &q, &never).unwrap();
        let uids = |list: &[ruthos_net::StudyResult]| {
            let mut u: Vec<String> = list.iter().map(|s| s.study_uid.clone()).collect();
            u.sort();
            u
        };
        assert_eq!(uids(&by_web), uids(&by_dimse), "{q:?}");
        assert!(!by_web.is_empty(), "{q:?}");
    }

    // What comes back is what went in.
    let q = StudyQuery {
        modality: "CT".into(),
        ..StudyQuery::default()
    };
    let study = client.find_studies(&q, &never).unwrap().studies.remove(0);
    let series = client.find_series(&study.study_uid, &never).unwrap();
    assert!(!series.is_empty());
    let out = std::env::temp_dir().join(format!("ruthos-orthanc-{}", std::process::id()));
    let outcome = client
        .retrieve(
            &study.study_uid,
            None,
            &out,
            &|_| false,
            &|_| false,
            &|_: RetrieveProgress| {},
            &never,
        )
        .unwrap();
    assert_eq!(sop_uids(&outcome.files), sop_uids(&samples()[..1]));
    let meta = ruthos_dicom::read_meta(&outcome.files[0]).unwrap();
    assert_eq!(meta.study_instance_uid, study.study_uid);
    std::fs::remove_dir_all(out).ok();
}

fn pixel_data(path: &PathBuf) -> Vec<u8> {
    let obj = dicom_object::open_file(path).unwrap();
    obj.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap().into_owned()
}

/// SEG acceptance 8: sent by C-STORE and fetched by C-GET, with pixel data
/// identical to the byte.
#[test]
fn a_seg_comes_back_from_orthanc_byte_for_byte() {
    let (Some(node), Some(seg)) = (dimse(), std::env::var_os("RUTHOS_ORTHANC_SEG")) else {
        eprintln!("no Orthanc set up: skipped");
        return;
    };
    let seg = PathBuf::from(seg);
    let never = AtomicBool::new(false);
    let sent = send_files(&node, "RUTHOS", std::slice::from_ref(&seg), &|_, _| {}, &never).unwrap();
    assert_eq!(sent.sent, 1, "{:?}", sent.failed);

    let obj = dicom_object::open_file(&seg).unwrap();
    let uid = |tag| {
        obj.element(tag)
            .unwrap()
            .to_str()
            .unwrap()
            .trim_end_matches('\0')
            .to_string()
    };
    let out = std::env::temp_dir().join(format!("ruthos-orthanc-seg-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let back = retrieve_series(
        &node,
        "RUTHOS",
        &uid(tags::STUDY_INSTANCE_UID),
        &uid(tags::SERIES_INSTANCE_UID),
        RetrieveMethod::Get,
        &out,
        &|_: RetrieveProgress| {},
        &never,
    )
    .unwrap();
    assert_eq!(back.files.len(), 1, "{:?}", back.progress);
    let (went, came) = (pixel_data(&seg), pixel_data(&back.files[0]));
    assert!(!went.is_empty());
    assert_eq!(went, came, "the pixel data changed on the way");
    eprintln!("{} bytes of SEG pixel data came back identical", came.len());
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn the_connection_test_passes() {
    let Some(client) = web() else {
        eprintln!("no Orthanc set up: skipped");
        return;
    };
    let (steps, _) = ruthos_net::dicomweb::test_connection(&client);
    assert!(steps.iter().all(|s| s.passed), "{steps:?}");
}
