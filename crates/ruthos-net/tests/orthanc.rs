//! Against a real Orthanc with its DICOMweb plugin. `RUTHOS_ORTHANC_WEB` is the
//! service root (`http://localhost:8042/dicom-web`) and `RUTHOS_ORTHANC_DIMSE` is
//! `host:port:AE`. CI runs an Orthanc; elsewhere these tests pass without one.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ruthos_net::dicomweb::{Basic, Transfer, WebClient, WebNode};
use ruthos_net::{find_studies, Node, RetrieveProgress, StudyQuery};

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

#[test]
fn a_transfer_syntax_the_server_cannot_give_falls_back() {
    let Some(client) = web() else {
        eprintln!("no Orthanc set up: skipped");
        return;
    };
    let (steps, _) = ruthos_net::dicomweb::test_connection(&client);
    assert!(steps.iter().all(|s| s.passed), "{steps:?}");
}
