//! Segmentation objects, written and read back against a synthetic CT series.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dicom_core::{dicom_value, DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};

use ruthos_dicom::sc::new_uid;
use ruthos_dicom::seg::{
    read_seg, source_series, write_seg, Algorithm, Code, Geometry, SegOptions, Segment, SegmentData, SourceSeries,
    Term, SOP_CLASS, TERMS,
};

struct Series {
    dir: PathBuf,
    files: Vec<PathBuf>,
    rows: usize,
    columns: usize,
}

impl Drop for Series {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `n` slices `step` apart, written shuffled so that nothing relies on file order.
fn ct(name: &str, rows: usize, columns: usize, n: usize, step: f64) -> Series {
    let dir = std::env::temp_dir().join(format!("ruthos-seg-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (study, series, frame) = (new_uid(), new_uid(), new_uid());
    let mut files = Vec::new();
    for k in 0..n {
        let z = (k * 7) % n;
        let sop = new_uid();
        let mut o = InMemDicomObject::new_empty();
        let mut s = |tag, vr, v: &str| o.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
        s(tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192");
        s(tags::SOP_CLASS_UID, VR::UI, uids::CT_IMAGE_STORAGE);
        s(tags::SOP_INSTANCE_UID, VR::UI, &sop);
        s(tags::STUDY_INSTANCE_UID, VR::UI, &study);
        s(tags::SERIES_INSTANCE_UID, VR::UI, &series);
        s(tags::FRAME_OF_REFERENCE_UID, VR::UI, &frame);
        s(tags::PATIENT_NAME, VR::PN, "HUGHES^BERTIE");
        s(tags::PATIENT_ID, VR::LO, "V12345");
        s(tags::PATIENT_SPECIES_DESCRIPTION, VR::LO, "Equine");
        s(tags::PATIENT_BIRTH_DATE, VR::DA, "20120515");
        s(tags::PATIENT_SEX, VR::CS, "M");
        s(tags::STUDY_TIME, VR::TM, "101500");
        s(tags::STUDY_ID, VR::SH, "1");
        s(tags::ACCESSION_NUMBER, VR::SH, "ACC1");
        s(tags::REFERRING_PHYSICIAN_NAME, VR::PN, "");
        s(tags::MODALITY, VR::CS, "CT");
        s(tags::STUDY_DATE, VR::DA, "20261105");
        s(tags::SLICE_THICKNESS, VR::DS, "1");
        s(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
        s(tags::IMAGE_ORIENTATION_PATIENT, VR::DS, "1\\0\\0\\0\\1\\0");
        s(tags::PIXEL_SPACING, VR::DS, "0.5\\0.75");
        s(
            tags::IMAGE_POSITION_PATIENT,
            VR::DS,
            &format!("-10\\20\\{}", z as f64 * step - 5.0),
        );
        for (tag, v) in [
            (tags::ROWS, rows as u16),
            (tags::COLUMNS, columns as u16),
            (tags::SAMPLES_PER_PIXEL, 1),
            (tags::BITS_ALLOCATED, 16),
            (tags::BITS_STORED, 16),
            (tags::HIGH_BIT, 15),
            (tags::PIXEL_REPRESENTATION, 1),
        ] {
            o.put(DataElement::new(tag, VR::US, PrimitiveValue::from(v)));
        }
        o.put(DataElement::new(
            tags::PIXEL_DATA,
            VR::OW,
            PrimitiveValue::U16(vec![0u16; rows * columns].into()),
        ));
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(uids::CT_IMAGE_STORAGE)
            .media_storage_sop_instance_uid(sop.clone())
            .build()
            .unwrap();
        let path = dir.join(format!("{sop}.dcm"));
        o.with_exact_meta(meta).write_to_file(&path).unwrap();
        files.push(path);
    }
    Series {
        dir,
        files,
        rows,
        columns,
    }
}

fn geometry(s: &SourceSeries) -> Geometry {
    Geometry {
        dims: [s.columns as usize, s.rows as usize, s.slices.len()],
        spacing: [s.pixel_spacing[1], s.pixel_spacing[0], s.spacing_between_slices],
        origin: s.slices[0].position,
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    }
}

fn segment(number: u16, label: &str, colour: [u8; 3], algorithm: Algorithm, term: &str) -> Segment {
    let t = Term::by_label(term).unwrap();
    Segment {
        number,
        label: label.to_string(),
        colour,
        algorithm,
        category: t.category(),
        kind: t.kind(),
    }
}

fn data(s: Segment, side: usize, voxels: &[(usize, usize)]) -> SegmentData {
    let mut slices: BTreeMap<usize, Vec<bool>> = BTreeMap::new();
    for (z, pixel) in voxels {
        slices.entry(*z).or_insert_with(|| vec![false; side])[*pixel] = true;
    }
    SegmentData { segment: s, slices }
}

fn options() -> SegOptions {
    SegOptions {
        series_uid: None,
        series_number: 900,
        instance_number: 1,
        series_description: "Ruthos segments".to_string(),
        creator: "BOWEN^MARK".to_string(),
        software_version: "0.1.10-alpha".to_string(),
    }
}

/// `dciodvfy` found these missing: the standard requires them, even empty, for
/// any patient who is not human.
#[test]
fn a_seg_of_an_animal_carries_what_the_standard_asks_of_one() {
    let required = [
        tags::PATIENT_BREED_DESCRIPTION,
        tags::PATIENT_BREED_CODE_SEQUENCE,
        tags::BREED_REGISTRATION_SEQUENCE,
        tags::RESPONSIBLE_PERSON,
        tags::RESPONSIBLE_ORGANIZATION,
        tags::PATIENT_SEX_NEUTERED,
    ];
    let segments = |side| {
        vec![data(
            segment(1, "Bone", [240, 200, 120], Algorithm::Manual, "Bone"),
            side,
            &[(1, 5)],
        )]
    };
    let series = ct("animal", 8, 8, 3, 2.0);
    let source = source_series(&series.files).unwrap();
    let out = write_seg(&series.dir, &source, &segments(64), &options()).unwrap();
    let written = dicom_object::open_file(&out.path).unwrap();
    for tag in required {
        assert!(written.get(tag).is_some(), "{tag}");
    }

    // A human patient's file does not gain them.
    let human: Vec<PathBuf> = series
        .files
        .iter()
        .map(|f| {
            rewritten(f, |o| {
                o.remove_element(tags::PATIENT_SPECIES_DESCRIPTION);
            })
        })
        .collect();
    let source = source_series(&human).unwrap();
    let out = write_seg(&series.dir, &source, &segments(64), &options()).unwrap();
    let written = dicom_object::open_file(&out.path).unwrap();
    for tag in required {
        assert!(written.get(tag).is_none(), "{tag}");
    }
}

/// Three segments, one with a non-ASCII label: every voxel, label, colour and
/// algorithm comes back (acceptance 4, at the level of the file).
#[test]
fn a_seg_comes_back_as_it_went_in() {
    let series = ct("round", 12, 16, 9, 2.0);
    let source = source_series(&series.files).unwrap();
    let side = series.rows * series.columns;
    let segments = vec![
        data(
            segment(
                1,
                "Maxillary bone",
                [240, 200, 120],
                Algorithm::SemiAutomatic("SAM 2.1 Tiny with Ruthos edge refinement".into()),
                "Bone",
            ),
            side,
            &[(0, 0), (0, 5), (3, side - 1), (8, 17)],
        ),
        data(
            segment(2, "Dent molaire è", [90, 170, 240], Algorithm::Manual, "Tooth"),
            side,
            &[(3, 2), (4, 3)],
        ),
        data(
            segment(
                3,
                "Mass",
                [240, 110, 100],
                Algorithm::SemiAutomatic("Ruthos region grow".into()),
                "Mass",
            ),
            side,
            &[(8, 100)],
        ),
    ];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    assert_eq!(out.frames, 3 + 2 + 1);

    let back = read_seg(&out.path).unwrap();
    assert!(back.refers_to(&source.series_uid));
    assert_eq!((back.rows, back.columns), (12, 16));
    assert_eq!(back.creator, "BOWEN^MARK");
    let placed = back.place(&geometry(&source)).unwrap();
    assert_eq!(placed.len(), 3);
    let [w, h] = [series.columns, series.rows];
    for (want, got) in segments.iter().zip(&placed) {
        assert_eq!(got.segment.label, want.segment.label);
        assert_eq!(got.segment.algorithm, want.segment.algorithm);
        assert_eq!(got.segment.kind, want.segment.kind);
        assert_eq!(got.segment.category, want.segment.category);
        for k in 0..3 {
            assert!(got.segment.colour[k].abs_diff(want.segment.colour[k]) <= 1);
        }
        let mut voxels: Vec<usize> = want
            .slices
            .iter()
            .flat_map(|(z, m)| {
                let z = *z;
                m.iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(move |(i, _)| i + w * h * z)
            })
            .collect();
        voxels.sort_unstable();
        assert_eq!(got.voxels, voxels);
    }
}

/// Each frame names the source image at its own position, with the file order
/// shuffled, and the header lists only the images used.
#[test]
fn each_frame_refers_to_the_slice_it_lies_on() {
    let series = ct("refs", 8, 8, 5, 3.0);
    let source = source_series(&series.files).unwrap();
    let positions: Vec<f64> = source.slices.iter().map(|s| s.position[2]).collect();
    assert!(positions.windows(2).all(|p| p[0] < p[1]), "{positions:?}");
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        64,
        &[(1, 0), (4, 0)],
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    let back = read_seg(&out.path).unwrap();
    for (frame, z) in back.frames.iter().zip([1, 4]) {
        assert_eq!(
            frame.source_instance.as_deref(),
            Some(source.slices[z].sop_instance.as_str())
        );
        assert_eq!(frame.position, source.slices[z].position);
    }
}

/// Acceptance 7: one small segment in a long series, and only its frames.
#[test]
fn a_small_segment_writes_only_its_own_frames() {
    let series = ct("small", 4, 4, 500, 0.5);
    let source = source_series(&series.files).unwrap();
    let side = 16;
    // Twenty voxels over two slices.
    let voxels: Vec<(usize, usize)> = (0..20).map(|i| (250 + i / 10, i % 16)).collect();
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        side,
        &voxels,
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    assert_eq!(out.frames, 2);
    let back = read_seg(&out.path).unwrap();
    assert_eq!(back.frames.len(), 2);
    assert_eq!(back.place(&geometry(&source)).unwrap()[0].voxels.len(), 20);
}

#[test]
fn nothing_to_save_is_said_so() {
    let series = ct("empty", 4, 4, 3, 1.0);
    let source = source_series(&series.files).unwrap();
    let mut slices = BTreeMap::new();
    slices.insert(1, vec![false; 16]);
    let segments = vec![SegmentData {
        segment: segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        slices,
    }];
    let e = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap_err();
    assert!(e.to_string().contains("nothing to save"), "{e}");
}

/// Acceptance 6, and its kin: what does not line up is refused and explained.
#[test]
fn a_seg_that_does_not_line_up_is_refused() {
    let series = ct("geom", 8, 8, 6, 2.0);
    let source = source_series(&series.files).unwrap();
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        64,
        &[(2, 9)],
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    let back = read_seg(&out.path).unwrap();
    let good = geometry(&source);
    assert!(back.place(&good).is_ok());

    let moved = Geometry {
        origin: [good.origin[0], good.origin[1], good.origin[2] + 1.0],
        ..good
    };
    assert!(back
        .place(&moved)
        .unwrap_err()
        .to_string()
        .contains("between the slices"));

    let shifted = Geometry {
        origin: [good.origin[0] + 0.3, good.origin[1], good.origin[2]],
        ..good
    };
    assert!(back
        .place(&shifted)
        .unwrap_err()
        .to_string()
        .contains("between the slices"));

    let turned = Geometry {
        axes: [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]],
        ..good
    };
    assert!(back.place(&turned).unwrap_err().to_string().contains("orientation"));

    let finer = Geometry {
        spacing: [0.375, 0.25, 2.0],
        ..good
    };
    assert!(back.place(&finer).unwrap_err().to_string().contains("pixel spacing"));

    let short = Geometry {
        dims: [8, 8, 2],
        ..good
    };
    assert!(back.place(&short).unwrap_err().to_string().contains("outside"));
}

/// A frame cropped from a slice is on whole voxels, so it is placed where it lies.
#[test]
fn a_cropped_frame_lands_on_its_own_voxels() {
    let series = ct("crop", 8, 8, 4, 2.0);
    let source = source_series(&series.files).unwrap();
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        64,
        &[(1, 0)],
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    let back = read_seg(&out.path).unwrap();
    // The same frame on a grid that starts two columns and a row further out.
    let g = geometry(&source);
    let wider = Geometry {
        dims: [10, 9, 4],
        origin: [
            g.origin[0] - 2.0 * g.spacing[0],
            g.origin[1] - g.spacing[1],
            g.origin[2],
        ],
        ..g
    };
    assert_eq!(back.place(&wider).unwrap()[0].voxels, vec![2 + 10 * (1 + 9)]);
}

fn rewritten(path: &Path, change: impl FnOnce(&mut InMemDicomObject)) -> PathBuf {
    let file = ruthos_dicom::open::open_any(path, None).unwrap();
    let mut obj: InMemDicomObject = file.into_inner();
    change(&mut obj);
    let meta = FileMetaTableBuilder::new()
        .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .media_storage_sop_class_uid(SOP_CLASS)
        .media_storage_sop_instance_uid("1.2.3")
        .build()
        .unwrap();
    let out = path.with_extension("changed.dcm");
    obj.with_exact_meta(meta).write_to_file(&out).unwrap();
    out
}

/// A file from elsewhere is not trusted: each of these is refused, none panics.
#[test]
fn a_broken_or_unread_kind_of_seg_is_refused() {
    let series = ct("bad", 8, 8, 3, 2.0);
    let source = source_series(&series.files).unwrap();
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        64,
        &[(1, 9)],
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    let refused = |change: Box<dyn FnOnce(&mut InMemDicomObject)>, words: &str| {
        let e = read_seg(&rewritten(&out.path, change)).unwrap_err().to_string();
        assert!(e.contains(words), "{e}");
    };
    refused(
        Box::new(|o| {
            o.put(DataElement::new(
                tags::SEGMENTATION_TYPE,
                VR::CS,
                dicom_value!(Str, "FRACTIONAL"),
            ));
        }),
        "probabilities",
    );
    refused(
        Box::new(|o| {
            o.put(DataElement::new(
                tags::PIXEL_DATA,
                VR::OB,
                PrimitiveValue::U8(vec![0u8; 2].into()),
            ));
        }),
        "cut short",
    );
    refused(
        Box::new(|o| {
            o.put(DataElement::new(tags::NUMBER_OF_FRAMES, VR::IS, dicom_value!(Str, "2")));
        }),
        "cut short",
    );
    refused(
        Box::new(|o| {
            o.remove_element(tags::SHARED_FUNCTIONAL_GROUPS_SEQUENCE);
        }),
        "orientation",
    );
    refused(
        Box::new(|o| {
            o.remove_element(tags::SEGMENT_SEQUENCE);
        }),
        "no segments",
    );
    refused(
        Box::new(|o| {
            o.put(DataElement::new(
                tags::SOP_CLASS_UID,
                VR::UI,
                dicom_value!(Str, uids::CT_IMAGE_STORAGE),
            ));
        }),
        "not a DICOM Segmentation",
    );
}

#[test]
fn every_term_offers_a_snomed_code() {
    for t in &TERMS {
        let c: Code = t.kind();
        assert!(!c.value.is_empty() && c.scheme == "SCT" && !c.meaning.is_empty());
    }
}

/// Files for `scripts/seg_interop.py`, which has other tools check them: set
/// `RUTHOS_SEG_INTEROP` to an empty folder. The script's own SEG is read here on
/// the second run.
#[test]
fn interoperates_with_other_tools() {
    let Some(folder) = std::env::var_os("RUTHOS_SEG_INTEROP") else {
        return;
    };
    let folder = PathBuf::from(folder);
    let foreign = folder.join("highdicom-seg.dcm");
    if foreign.is_file() {
        let ct_files: Vec<PathBuf> = std::fs::read_dir(folder.join("ct"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let source = source_series(&ct_files).unwrap();
        let back = read_seg(&foreign).unwrap();
        assert!(back.refers_to(&source.series_uid));
        let placed = back.place(&geometry(&source)).unwrap();
        let want = std::fs::read_to_string(folder.join("highdicom-voxels.txt")).unwrap();
        for (line, got) in want.lines().zip(&placed) {
            let (label, voxels) = line.split_once(':').unwrap();
            let voxels: Vec<usize> = voxels.split_whitespace().map(|v| v.parse().unwrap()).collect();
            assert_eq!(got.segment.label, label);
            assert_eq!(got.voxels, voxels, "{label}");
            eprintln!("{label}: {} voxels identical, {:?}", voxels.len(), got.segment.kind);
        }
        assert_eq!(want.lines().count(), placed.len());
        return;
    }
    let ct_dir = folder.join("ct");
    let series = ct("interop", 32, 40, 12, 2.0);
    std::fs::create_dir_all(&ct_dir).unwrap();
    for f in &series.files {
        std::fs::copy(f, ct_dir.join(f.file_name().unwrap())).unwrap();
    }
    let source = source_series(&series.files).unwrap();
    let side = 32 * 40;
    let block = |z: usize, from: usize, n: usize| (0..n).map(move |i| (z, from + i * 41));
    let segments = vec![
        data(
            segment(
                1,
                "Maxilla",
                [240, 200, 120],
                Algorithm::SemiAutomatic("SAM 2.1 Tiny with Ruthos edge refinement".into()),
                "Bone",
            ),
            side,
            &block(3, 50, 20).chain(block(4, 50, 20)).collect::<Vec<_>>(),
        ),
        data(
            segment(2, "Tooth 106", [90, 170, 240], Algorithm::Manual, "Tooth"),
            side,
            &block(5, 200, 10).collect::<Vec<_>>(),
        ),
        data(
            segment(
                3,
                "Mass",
                [240, 110, 100],
                Algorithm::SemiAutomatic("Ruthos region grow".into()),
                "Mass",
            ),
            side,
            &block(9, 300, 8).collect::<Vec<_>>(),
        ),
    ];
    let out = write_seg(&folder, &source, &segments, &options()).unwrap();
    std::fs::rename(&out.path, folder.join("ruthos-seg.dcm")).unwrap();
    let mut listing = String::new();
    for s in &segments {
        let voxels: Vec<String> = s
            .slices
            .iter()
            .flat_map(|(z, m)| {
                let z = *z;
                m.iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(move |(i, _)| (i + side * z).to_string())
            })
            .collect();
        listing.push_str(&format!("{}:{}\n", s.segment.label, voxels.join(" ")));
    }
    std::fs::write(folder.join("ruthos-voxels.txt"), listing).unwrap();
}

/// The image path meets a SEG when the database makes a thumbnail. One bit a
/// voxel is not a picture it need show, but it must not bring anything down.
#[test]
fn decoding_a_seg_as_an_image_does_not_panic() {
    let series = ct("decode", 8, 8, 3, 2.0);
    let source = source_series(&series.files).unwrap();
    let segments = vec![data(
        segment(1, "A", [1, 2, 3], Algorithm::Manual, "Bone"),
        64,
        &[(1, 9), (2, 9)],
    )];
    let out = write_seg(&series.dir.join("seg"), &source, &segments, &options()).unwrap();
    let meta = ruthos_dicom::read_meta(&out.path).unwrap();
    assert_eq!(meta.modality, "SEG");
    eprintln!(
        "decoded: {:?}",
        ruthos_dicom::decode_frame(&out.path, 0).map(|f| (f.width, f.height))
    );
}
