//! Saving MPR planes into the study: a small synthetic CT series is
//! imported, built into a volume, resliced and saved back as a new series.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use dicom_core::value::PrimitiveValue;
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_core::import::{import_file, FileOutcome, ImportMode};
use ruthos_core::mpr_export::{new_uid, save_planes, stack, SavedPlane};
use ruthos_core::volume::{Plane, SlabMode, Volume};
use ruthos_core::{DataPaths, Database, RoiRecord};

fn write_slice(dir: &Path, study: &str, series: &str, k: usize) -> PathBuf {
    let (w, h) = (32u16, 24u16);
    let values: Vec<i16> = (0..h as i32)
        .flat_map(|y| (0..w as i32).map(move |x| (x + 10 * y + 100 * k as i32 - 500) as i16))
        .collect();
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut o = InMemDicomObject::new_empty();
    let s = |o: &mut InMemDicomObject, tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
    let sop = new_uid();
    s(&mut o, tags::SOP_CLASS_UID, VR::UI, uids::CT_IMAGE_STORAGE);
    s(&mut o, tags::SOP_INSTANCE_UID, VR::UI, &sop);
    s(&mut o, tags::STUDY_INSTANCE_UID, VR::UI, study);
    s(&mut o, tags::SERIES_INSTANCE_UID, VR::UI, series);
    s(&mut o, tags::MODALITY, VR::CS, "CT");
    s(&mut o, tags::PATIENT_NAME, VR::PN, "PHANTOM^SAVE");
    s(&mut o, tags::PATIENT_ID, VR::LO, "P1");
    s(&mut o, tags::SERIES_NUMBER, VR::IS, "1");
    s(&mut o, tags::INSTANCE_NUMBER, VR::IS, &(k + 1).to_string());
    s(
        &mut o,
        tags::IMAGE_POSITION_PATIENT,
        VR::DS,
        &format!("0\\0\\{}", 2 * k),
    );
    s(&mut o, tags::IMAGE_ORIENTATION_PATIENT, VR::DS, "1\\0\\0\\0\\1\\0");
    s(&mut o, tags::PIXEL_SPACING, VR::DS, "0.5\\0.5");
    s(&mut o, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    s(&mut o, tags::RESCALE_INTERCEPT, VR::DS, "0");
    s(&mut o, tags::RESCALE_SLOPE, VR::DS, "1");
    let us = |o: &mut InMemDicomObject, tag, v: u16| o.put(DataElement::new(tag, VR::US, dicom_value!(U16, [v])));
    us(&mut o, tags::ROWS, h);
    us(&mut o, tags::COLUMNS, w);
    us(&mut o, tags::SAMPLES_PER_PIXEL, 1);
    us(&mut o, tags::BITS_ALLOCATED, 16);
    us(&mut o, tags::BITS_STORED, 16);
    us(&mut o, tags::HIGH_BIT, 15);
    us(&mut o, tags::PIXEL_REPRESENTATION, 1);
    o.put(DataElement::new(tags::PIXEL_DATA, VR::OW, PrimitiveValue::from(bytes)));
    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(uids::CT_IMAGE_STORAGE)
        .media_storage_sop_instance_uid(sop.as_str())
        .build()
        .unwrap();
    let f = dir.join(format!("s{k}.dcm"));
    o.with_exact_meta(meta).write_to_file(&f).unwrap();
    f
}

#[test]
fn a_coronal_plane_and_a_stack_are_saved_into_the_study_with_measurements() {
    let root = std::env::temp_dir().join(format!("ruthos-mprsave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let paths = DataPaths::new(root.join("archive"));
    paths.ensure().unwrap();
    let db = Database::open(&paths).unwrap();
    let (study, series) = (new_uid(), new_uid());
    let mut stored = Vec::new();
    for k in 0..6 {
        let f = write_slice(&src, &study, &series, k);
        match import_file(&db, &paths, &f, ImportMode::Copy).unwrap() {
            FileOutcome::Imported { image_id, .. } => stored.push(image_id),
            other => panic!("{other:?}"),
        }
    }
    let files: Vec<PathBuf> = stored
        .iter()
        .map(|id| paths.resolve(&db.image(*id).unwrap().unwrap().path))
        .collect();
    let volume = Volume::load(&files, &|_, _| {}, &AtomicBool::new(false)).unwrap();

    let coronal = Plane {
        centre: volume.centre(),
        right: [1.0, 0.0, 0.0],
        down: [0.0, 0.0, -1.0],
        width: 40,
        height: 30,
        pixel_mm: 0.5,
        slab_mm: 0.0,
        slab_mode: SlabMode::Mip,
    };
    let caliper = RoiRecord {
        id: 0,
        image_id: 0,
        frame: 0,
        kind: "length".into(),
        points: vec![[5.0, 5.0], [25.0, 5.0]],
        name: Some("Width".into()),
        value: Some(10.0),
        unit: "mm".into(),
        basis: None,
        def_key: None,
    };
    let saved = save_planes(
        &db,
        &paths,
        &files[0],
        &volume,
        &[SavedPlane {
            plane: coronal,
            rois: vec![caliper],
        }],
        "MPR coronal",
        (40.0, 400.0),
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(saved.image_ids.len(), 1);
    let img = db.image(saved.image_ids[0]).unwrap().unwrap();
    let new_series = db.series(img.series_id).unwrap().unwrap();
    let study_id = db.study_id_for_uid(&study).unwrap().unwrap();
    assert_eq!(new_series.study_id, study_id, "saved into the same study");
    assert_eq!(new_series.series_description, "MPR coronal");
    // The saved image has the plane's geometry and decodes.
    let meta = ruthos_dicom::read_meta(&paths.resolve(&img.path)).unwrap();
    assert_eq!((meta.columns, meta.rows), (40, 30));
    assert_eq!(meta.pixel_spacing_tag, Some([0.5, 0.5]));
    let o = meta.image_orientation.unwrap();
    assert!((o[5] + 1.0).abs() < 1e-6, "down is towards the feet: {o:?}");
    let frame = ruthos_dicom::decode_frame(&paths.resolve(&img.path), 0).unwrap();
    assert!(frame.min < 0.0, "negative values survive");
    // The measurement came with it.
    let rois = db.rois(img.id, 0).unwrap();
    assert_eq!(rois.len(), 1);
    assert_eq!(rois[0].name.as_deref(), Some("Width"));

    // A stack across the volume, 2 mm apart: the volume spans 10 mm.
    let planes = stack(
        &volume,
        &Plane {
            down: [0.0, 1.0, 0.0],
            ..coronal
        },
        2.0,
        None,
    );
    assert_eq!(planes.len(), 6, "{}", planes.len());
    let saved: Vec<SavedPlane> = planes
        .into_iter()
        .map(|plane| SavedPlane {
            plane,
            rois: Vec::new(),
        })
        .collect();
    let r = save_planes(
        &db,
        &paths,
        &files[0],
        &volume,
        &saved,
        "MPR axial",
        (40.0, 400.0),
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(r.image_ids.len(), 6);
    let _ = std::fs::remove_dir_all(&root);
}
