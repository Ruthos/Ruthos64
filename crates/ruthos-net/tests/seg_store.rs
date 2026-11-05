//! A SEG goes over the network like any other series, with its voxels unchanged:
//! sent by C-STORE to a listener that accepts only known kinds of object.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dicom_core::{dicom_value, DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_dicom::open::open_any;
use ruthos_dicom::sc::new_uid;
use ruthos_dicom::seg::{read_seg, source_series, write_seg, Algorithm, SegOptions, Segment, SegmentData, Term};
use ruthos_net::scp::AllowEntry;
use ruthos_net::{send_files, ListenerConfig, Node, StoreScp};

fn ct(dir: &std::path::Path) -> Vec<PathBuf> {
    let (study, series) = (new_uid(), new_uid());
    (0..4)
        .map(|z| {
            let sop = new_uid();
            let mut o = InMemDicomObject::new_empty();
            let mut s = |tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
            s(tags::SOP_CLASS_UID, VR::UI, uids::CT_IMAGE_STORAGE);
            s(tags::SOP_INSTANCE_UID, VR::UI, &sop);
            s(tags::STUDY_INSTANCE_UID, VR::UI, &study);
            s(tags::SERIES_INSTANCE_UID, VR::UI, &series);
            s(tags::MODALITY, VR::CS, "CT");
            s(tags::IMAGE_ORIENTATION_PATIENT, VR::DS, "1\\0\\0\\0\\1\\0");
            s(tags::PIXEL_SPACING, VR::DS, "1\\1");
            s(tags::IMAGE_POSITION_PATIENT, VR::DS, &format!("0\\0\\{z}"));
            for (tag, v) in [(tags::ROWS, 16u16), (tags::COLUMNS, 16)] {
                o.put(DataElement::new(tag, VR::US, PrimitiveValue::from(v)));
            }
            let meta = FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .media_storage_sop_class_uid(uids::CT_IMAGE_STORAGE)
                .media_storage_sop_instance_uid(sop.clone())
                .build()
                .unwrap();
            let path = dir.join(format!("{sop}.dcm"));
            o.with_exact_meta(meta).write_to_file(&path).unwrap();
            path
        })
        .collect()
}

fn pixels(path: &std::path::Path) -> Vec<u8> {
    open_any(path, None)
        .unwrap()
        .element(tags::PIXEL_DATA)
        .unwrap()
        .value()
        .to_bytes()
        .unwrap()
        .to_vec()
}

#[test]
fn a_seg_is_sent_with_its_voxels_unchanged() {
    let root = std::env::temp_dir().join(format!("ruthos-seg-net-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let files = ct(&root);
    let source = source_series(&files).unwrap();
    let t = Term::by_label("Bone").unwrap();
    let mut slices = BTreeMap::new();
    slices.insert(1usize, (0..256).map(|i| i % 3 == 0).collect::<Vec<bool>>());
    slices.insert(2usize, (0..256).map(|i| i % 7 == 0).collect::<Vec<bool>>());
    let data = vec![SegmentData {
        segment: Segment {
            number: 1,
            label: "Bone".to_string(),
            colour: [1, 2, 3],
            algorithm: Algorithm::Manual,
            category: t.category(),
            kind: t.kind(),
        },
        slices,
    }];
    let written = write_seg(&root.join("seg"), &source, &data, &SegOptions::default()).unwrap();

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let inbox = root.join("inbox");
    let allow = Arc::new(Mutex::new(vec![AllowEntry {
        name: "Test".into(),
        ae_title: "SENDER".into(),
        address: String::new(),
    }]));
    let mut scp = StoreScp::start(ListenerConfig {
        port,
        ae_title: "RUTHOS".into(),
        out_dir: inbox.clone(),
        allow,
        promiscuous: false,
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let node = Node {
        host: "127.0.0.1".into(),
        port,
        ae_title: "RUTHOS".into(),
    };
    let outcome = send_files(
        &node,
        "SENDER",
        std::slice::from_ref(&written.path),
        &|_, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(outcome.failed.is_empty(), "{outcome:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let received = loop {
        let found = std::fs::read_dir(&inbox).ok().and_then(|mut d| {
            d.find_map(|e| {
                let p = e.ok()?.path();
                (p.is_file() && read_seg(&p).is_ok()).then_some(p)
            })
        });
        if let Some(p) = found {
            break p;
        }
        assert!(std::time::Instant::now() < deadline, "nothing arrived");
        std::thread::sleep(Duration::from_millis(50));
    };
    scp.stop();
    assert_eq!(pixels(&received), pixels(&written.path), "byte for byte");
    let back = read_seg(&received).unwrap();
    assert_eq!(back.segments[0].label, "Bone");
    assert_eq!(back.frames.len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}
