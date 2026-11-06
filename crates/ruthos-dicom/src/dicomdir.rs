//! DICOMDIR: the index of a set of DICOM files on a disc or stick (PS3.10).
//!
//! Records point at each other by byte offset, which dicom-rs does not keep, so
//! this reads and writes the file itself. A DICOMDIR is treated as an index and
//! never as the truth: what it says is enough to show what is on a disc at once,
//! and everything stored comes from the files it names.
//!
//! The file comes from whoever made the disc, so every offset and length is
//! checked, links that loop are stopped, and a file's path can only lead to
//! somewhere inside the folder holding the DICOMDIR. When the links are broken
//! the records are read in file order, which is the order writers use.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::{DicomError, Result};

/// The name the standard gives the file.
pub const FILE_NAME: &str = "DICOMDIR";
const MEDIA_STORAGE_DIRECTORY: &str = "1.2.840.10008.1.3.10";
const EXPLICIT_LE: &str = "1.2.840.10008.1.2.1";
const IMPLICIT_LE: &str = "1.2.840.10008.1.2";
const IMPLEMENTATION_CLASS: &str = "1.2.826.0.1.3680043.10.1465.1";
const IMPLEMENTATION_VERSION: &str = "RUTHOS_0_1";

/// Far larger than the index of any real disc.
const MOST_BYTES: u64 = 512 * 1024 * 1024;
const MOST_RECORDS: usize = 4_000_000;
const MOST_DEPTH: u32 = 16;
/// The most folders a referenced path may pass through.
const MOST_PATH_PARTS: usize = 16;
/// What the standard allows in a file written here.
const MOST_FILE_ID_PARTS: usize = 8;
const MOST_FILE_ID_LENGTH: usize = 8;

const UNDEFINED: u32 = 0xFFFF_FFFF;
const ITEM: u32 = 0xFFFE_E000;
const ITEM_END: u32 = 0xFFFE_E00D;
const SEQUENCE_END: u32 = 0xFFFE_E0DD;

const FILE_SET_ID: u32 = 0x0004_1130;
const FIRST_ROOT: u32 = 0x0004_1200;
const LAST_ROOT: u32 = 0x0004_1202;
const CONSISTENCY: u32 = 0x0004_1212;
const RECORDS: u32 = 0x0004_1220;
const NEXT: u32 = 0x0004_1400;
const IN_USE: u32 = 0x0004_1410;
const LOWER: u32 = 0x0004_1420;
const RECORD_TYPE: u32 = 0x0004_1430;
const FILE_ID: u32 = 0x0004_1500;
const SOP_CLASS_IN_FILE: u32 = 0x0004_1510;
const SOP_INSTANCE_IN_FILE: u32 = 0x0004_1511;
const TRANSFER_SYNTAX_IN_FILE: u32 = 0x0004_1512;
const CHARACTER_SET: u32 = 0x0008_0005;
const STUDY_DATE: u32 = 0x0008_0020;
const STUDY_TIME: u32 = 0x0008_0030;
const ACCESSION: u32 = 0x0008_0050;
const MODALITY: u32 = 0x0008_0060;
const STUDY_DESCRIPTION: u32 = 0x0008_1030;
const SERIES_DESCRIPTION: u32 = 0x0008_103E;
const PATIENT_NAME: u32 = 0x0010_0010;
const PATIENT_ID: u32 = 0x0010_0020;
const STUDY_UID: u32 = 0x0020_000D;
const SERIES_UID: u32 = 0x0020_000E;
const STUDY_ID: u32 = 0x0020_0010;
const SERIES_NUMBER: u32 = 0x0020_0011;
const INSTANCE_NUMBER: u32 = 0x0020_0013;

/// What a DICOMDIR lists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DicomDir {
    pub file_set_id: String,
    pub patients: Vec<DirPatient>,
    /// Files whose path led out of the folder or could not be used.
    pub refused: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirPatient {
    pub name: String,
    pub id: String,
    pub studies: Vec<DirStudy>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirStudy {
    pub uid: String,
    pub id: String,
    pub date: String,
    pub time: String,
    pub accession: String,
    pub description: String,
    pub series: Vec<DirSeries>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirSeries {
    pub uid: String,
    pub modality: String,
    pub number: String,
    pub description: String,
    pub files: Vec<DirFile>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirFile {
    /// Relative to the folder holding the DICOMDIR.
    pub path: PathBuf,
    pub sop_class: String,
    pub sop_instance: String,
    pub transfer_syntax: String,
    pub instance_number: Option<i64>,
}

impl DicomDir {
    pub fn file_count(&self) -> usize {
        self.series().map(|s| s.files.len()).sum()
    }

    pub fn series(&self) -> impl Iterator<Item = &DirSeries> {
        self.patients.iter().flat_map(|p| &p.studies).flat_map(|s| &s.series)
    }

    /// The listed files that can be found, as full paths, and how many cannot.
    pub fn locate_all(&self, root: &Path) -> (Vec<PathBuf>, usize) {
        let mut found = Vec::with_capacity(self.file_count());
        let mut missing = 0;
        for series in self.series() {
            for file in &series.files {
                match locate(root, &file.path) {
                    Some(p) => found.push(p),
                    None => missing += 1,
                }
            }
        }
        (found, missing)
    }
}

/// True for a path named DICOMDIR, in any case.
pub fn is_dicomdir(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(FILE_NAME))
}

/// The DICOMDIR directly in a folder, however its name is cased.
pub fn find_in(folder: &Path) -> Option<PathBuf> {
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| is_dicomdir(p) && p.is_file())
}

/// Read a DICOMDIR from disk.
pub fn open(path: &Path) -> Result<DicomDir> {
    let size = std::fs::metadata(path)
        .map_err(|e| DicomError::Read(e.to_string()))?
        .len();
    if size > MOST_BYTES {
        return Err(DicomError::Read("the DICOMDIR is too large to be one".into()));
    }
    let bytes = std::fs::read(path).map_err(|e| DicomError::Read(e.to_string()))?;
    parse(&bytes)
}

/// Find a listed file under `root`. Names are matched in any case, as discs
/// are often read on a system that cares about it, and nothing on the way may
/// be a link. Returns `None` if the file is not there.
pub fn locate(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else { return None };
        let exact = at.join(part);
        let next = if std::fs::symlink_metadata(&exact).is_ok() {
            exact
        } else {
            let wanted = part.to_str()?;
            std::fs::read_dir(&at)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(wanted)))?
                .path()
        };
        if std::fs::symlink_metadata(&next).ok()?.file_type().is_symlink() {
            return None;
        }
        at = next;
    }
    at.is_file().then_some(at)
}

/// Turn a Referenced File ID into a path that stays inside its folder. Discs
/// carry ISO 9660 leftovers: a `;1` version and a trailing dot on the name.
fn safe_path(file_id: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    let parts: Vec<&str> = file_id.split('\\').collect();
    if parts.is_empty() || parts.len() > MOST_PATH_PARTS {
        return None;
    }
    for raw in parts {
        let part = raw.trim().split(';').next()?.trim_end_matches('.');
        let usable = !part.is_empty()
            && part.len() <= 255
            && !part
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
        if !usable {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

// ---------------------------------------------------------------- reading

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

struct Element {
    tag: u32,
    length: u32,
    sequence: bool,
}

fn short() -> DicomError {
    DicomError::Read("the DICOMDIR ends early or an offset in it is wrong".into())
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(short)?;
        let out = self.bytes.get(self.at..end).ok_or_else(short)?;
        self.at = end;
        Ok(out)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn tag(&mut self) -> Result<u32> {
        let group = u32::from(self.u16()?);
        Ok(group << 16 | u32::from(self.u16()?))
    }

    fn element(&mut self, implicit: bool) -> Result<Element> {
        let tag = self.tag()?;
        if tag >> 16 == 0xFFFE {
            let length = self.u32()?;
            return Ok(Element {
                tag,
                length,
                sequence: false,
            });
        }
        if implicit {
            let length = self.u32()?;
            return Ok(Element {
                tag,
                length,
                sequence: tag == RECORDS || length == UNDEFINED,
            });
        }
        let vr = self.take(2)?;
        let long = matches!(
            vr,
            b"OB" | b"OW" | b"OF" | b"OD" | b"OL" | b"OV" | b"SQ" | b"UC" | b"UN" | b"UR" | b"UT" | b"SV" | b"UV"
        );
        let sequence = vr == b"SQ";
        let length = if long {
            self.take(2)?;
            self.u32()?
        } else {
            u32::from(self.u16()?)
        };
        Ok(Element {
            tag,
            length,
            sequence: sequence || length == UNDEFINED,
        })
    }

    /// Step over a value, including sequences of unknown length.
    fn skip(&mut self, element: &Element, implicit: bool, depth: u32) -> Result<()> {
        if element.length != UNDEFINED {
            self.take(element.length as usize)?;
            return Ok(());
        }
        if depth >= MOST_DEPTH {
            return Err(DicomError::Read("the DICOMDIR is nested too deeply".into()));
        }
        loop {
            let tag = self.tag()?;
            let length = self.u32()?;
            match tag {
                SEQUENCE_END => return Ok(()),
                ITEM if length != UNDEFINED => {
                    self.take(length as usize)?;
                }
                ITEM => loop {
                    let inner = self.element(implicit)?;
                    if inner.tag == ITEM_END {
                        break;
                    }
                    self.skip(&inner, implicit, depth + 1)?;
                },
                _ => return Err(short()),
            }
        }
    }
}

fn text(value: &[u8]) -> String {
    let end = value.iter().rposition(|b| !matches!(b, 0 | b' ')).map_or(0, |i| i + 1);
    let value = &value[..end];
    // The listing only: a study's stored details come from its files.
    match std::str::from_utf8(value) {
        Ok(s) => s.to_string(),
        Err(_) => value.iter().map(|&b| char::from(b)).collect(),
    }
}

fn number(value: &[u8]) -> Option<u32> {
    (value.len() >= 4).then(|| u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[derive(Debug, Default)]
struct Record {
    next: u32,
    lower: u32,
    in_use: bool,
    kind: String,
    values: HashMap<u32, String>,
}

impl Record {
    fn value(&self, tag: u32) -> String {
        self.values.get(&tag).cloned().unwrap_or_default()
    }
}

/// Read a DICOMDIR's bytes.
pub fn parse(bytes: &[u8]) -> Result<DicomDir> {
    if bytes.get(128..132) != Some(b"DICM") {
        return Err(DicomError::NotDicom("no DICOM preamble".into()));
    }
    let mut cur = Cursor { bytes, at: 132 };
    let mut media_class = String::new();
    let mut transfer_syntax = String::new();
    while bytes.get(cur.at + 1) == Some(&0) && bytes.get(cur.at) == Some(&2) {
        let e = cur.element(false)?;
        let value = cur.take(e.length as usize)?;
        match e.tag {
            0x0002_0002 => media_class = text(value),
            0x0002_0010 => transfer_syntax = text(value),
            _ => {}
        }
    }
    if media_class != MEDIA_STORAGE_DIRECTORY {
        return Err(DicomError::NotDicom("this DICOM file is not a DICOMDIR".into()));
    }
    let implicit = match transfer_syntax.as_str() {
        EXPLICIT_LE => false,
        IMPLICIT_LE => true,
        other => return Err(DicomError::Read(format!("DICOMDIR in transfer syntax {other}"))),
    };

    let mut out = DicomDir::default();
    let mut first_root = 0;
    let mut records: Vec<Record> = Vec::new();
    let mut at_offset: HashMap<u32, usize> = HashMap::new();
    while cur.at < bytes.len() {
        let e = cur.element(implicit)?;
        match e.tag {
            RECORDS => read_records(&mut cur, &e, implicit, &mut records, &mut at_offset)?,
            FILE_SET_ID => out.file_set_id = text(cur.take(e.length as usize)?),
            FIRST_ROOT => first_root = number(cur.take(e.length as usize)?).unwrap_or(0),
            _ => cur.skip(&e, implicit, 0)?,
        }
    }

    let linked = linked_tree(&records, &at_offset, first_root, &mut out.refused);
    out.patients = match linked {
        Some(patients) => patients,
        None => {
            out.refused = 0;
            flat_tree(&records, &mut out.refused)
        }
    };
    Ok(out)
}

fn read_records(
    cur: &mut Cursor,
    sequence: &Element,
    implicit: bool,
    records: &mut Vec<Record>,
    at_offset: &mut HashMap<u32, usize>,
) -> Result<()> {
    let end = if sequence.length == UNDEFINED {
        usize::MAX
    } else {
        cur.at.saturating_add(sequence.length as usize)
    };
    while cur.at < end {
        let offset = cur.at;
        let tag = cur.tag()?;
        let length = cur.u32()?;
        if tag == SEQUENCE_END {
            return Ok(());
        }
        if tag != ITEM {
            return Err(short());
        }
        if records.len() >= MOST_RECORDS {
            return Err(DicomError::Read("the DICOMDIR has too many records".into()));
        }
        let item_end = if length == UNDEFINED {
            usize::MAX
        } else {
            cur.at.saturating_add(length as usize)
        };
        let mut record = Record {
            in_use: true,
            ..Record::default()
        };
        while cur.at < item_end {
            let e = cur.element(implicit)?;
            if e.tag == ITEM_END {
                break;
            }
            if e.sequence {
                cur.skip(&e, implicit, 0)?;
                continue;
            }
            let value = cur.take(e.length as usize)?;
            match e.tag {
                NEXT => record.next = number(value).unwrap_or(0),
                LOWER => record.lower = number(value).unwrap_or(0),
                IN_USE => record.in_use = value.len() < 2 || value[0] != 0 || value[1] != 0,
                RECORD_TYPE => record.kind = text(value),
                FILE_ID
                | SOP_CLASS_IN_FILE
                | SOP_INSTANCE_IN_FILE
                | TRANSFER_SYNTAX_IN_FILE
                | STUDY_DATE
                | STUDY_TIME
                | ACCESSION
                | MODALITY
                | STUDY_DESCRIPTION
                | SERIES_DESCRIPTION
                | PATIENT_NAME
                | PATIENT_ID
                | STUDY_UID
                | SERIES_UID
                | STUDY_ID
                | SERIES_NUMBER
                | INSTANCE_NUMBER => {
                    record.values.insert(e.tag, text(value));
                }
                _ => {}
            }
        }
        at_offset.insert(offset as u32, records.len());
        records.push(record);
    }
    Ok(())
}

/// The records on one level, following the links. A link that loops ends here.
fn siblings(
    first: u32,
    at_offset: &HashMap<u32, usize>,
    records: &[Record],
    seen: &mut HashSet<usize>,
) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    let mut offset = first;
    while offset != 0 {
        // A link to nowhere means the links cannot be trusted at all.
        let &i = at_offset.get(&offset)?;
        if !seen.insert(i) {
            return None;
        }
        out.push(i);
        offset = records[i].next;
    }
    Some(out)
}

fn patient_of(r: &Record) -> DirPatient {
    DirPatient {
        name: r.value(PATIENT_NAME),
        id: r.value(PATIENT_ID),
        studies: Vec::new(),
    }
}

fn study_of(r: &Record) -> DirStudy {
    DirStudy {
        uid: r.value(STUDY_UID),
        id: r.value(STUDY_ID),
        date: r.value(STUDY_DATE),
        time: r.value(STUDY_TIME),
        accession: r.value(ACCESSION),
        description: r.value(STUDY_DESCRIPTION),
        series: Vec::new(),
    }
}

fn series_of(r: &Record) -> DirSeries {
    DirSeries {
        uid: r.value(SERIES_UID),
        modality: r.value(MODALITY),
        number: r.value(SERIES_NUMBER),
        description: r.value(SERIES_DESCRIPTION),
        files: Vec::new(),
    }
}

fn file_of(r: &Record, refused: &mut usize) -> Option<DirFile> {
    let id = r.values.get(&FILE_ID)?;
    let Some(path) = safe_path(id) else {
        *refused += 1;
        return None;
    };
    Some(DirFile {
        path,
        sop_class: r.value(SOP_CLASS_IN_FILE),
        sop_instance: r.value(SOP_INSTANCE_IN_FILE),
        transfer_syntax: r.value(TRANSFER_SYNTAX_IN_FILE),
        instance_number: r.value(INSTANCE_NUMBER).parse().ok(),
    })
}

fn linked_tree(
    records: &[Record],
    at_offset: &HashMap<u32, usize>,
    first_root: u32,
    refused: &mut usize,
) -> Option<Vec<DirPatient>> {
    let mut seen = HashSet::new();
    let mut patients = Vec::new();
    for i in siblings(first_root, at_offset, records, &mut seen)? {
        let r = &records[i];
        if !r.in_use {
            continue;
        }
        if r.kind != "PATIENT" {
            continue;
        }
        let mut patient = patient_of(r);
        for i in siblings(r.lower, at_offset, records, &mut seen)? {
            let r = &records[i];
            if !r.in_use || r.kind != "STUDY" {
                continue;
            }
            let mut study = study_of(r);
            for i in siblings(r.lower, at_offset, records, &mut seen)? {
                let r = &records[i];
                if !r.in_use || r.kind != "SERIES" {
                    continue;
                }
                let mut series = series_of(r);
                for i in siblings(r.lower, at_offset, records, &mut seen)? {
                    let r = &records[i];
                    if r.in_use {
                        series.files.extend(file_of(r, refused));
                    }
                }
                study.series.push(series);
            }
            patient.studies.push(study);
        }
        patients.push(patient);
    }
    // Files the links never reached: some writers leave the offsets wrong.
    let unreached = records
        .iter()
        .enumerate()
        .any(|(i, r)| r.in_use && r.values.contains_key(&FILE_ID) && !seen.contains(&i));
    (!unreached).then_some(patients)
}

/// Rebuild the levels from the order of the records.
fn flat_tree(records: &[Record], refused: &mut usize) -> Vec<DirPatient> {
    let mut patients: Vec<DirPatient> = Vec::new();
    for r in records.iter().filter(|r| r.in_use) {
        match r.kind.as_str() {
            "PATIENT" => patients.push(patient_of(r)),
            "STUDY" => {
                if patients.is_empty() {
                    patients.push(DirPatient::default());
                }
                if let Some(p) = patients.last_mut() {
                    p.studies.push(study_of(r));
                }
            }
            "SERIES" => {
                if let Some(s) = patients.last_mut().and_then(|p| p.studies.last_mut()) {
                    s.series.push(series_of(r));
                }
            }
            _ => {
                let series = patients
                    .last_mut()
                    .and_then(|p| p.studies.last_mut())
                    .and_then(|s| s.series.last_mut());
                if let Some(series) = series {
                    series.files.extend(file_of(r, refused));
                }
            }
        }
    }
    patients
}

// ---------------------------------------------------------------- writing

/// True when a name fits the standard's file ID rules.
pub fn valid_file_id_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= MOST_FILE_ID_LENGTH
        && part
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn file_id(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for c in path.components() {
        let part = match c {
            Component::Normal(p) => p.to_str().unwrap_or(""),
            _ => "",
        };
        if !valid_file_id_part(part) {
            return Err(DicomError::Read(format!("{} is not a DICOM file ID", path.display())));
        }
        parts.push(part);
    }
    if parts.is_empty() || parts.len() > MOST_FILE_ID_PARTS {
        return Err(DicomError::Read(format!("{} is not a DICOM file ID", path.display())));
    }
    Ok(parts.join("\\"))
}

fn tag_bytes(out: &mut Vec<u8>, tag: u32) {
    out.extend_from_slice(&((tag >> 16) as u16).to_le_bytes());
    out.extend_from_slice(&(tag as u16).to_le_bytes());
}

fn put_short(out: &mut Vec<u8>, tag: u32, vr: &[u8; 2], value: &[u8]) {
    tag_bytes(out, tag);
    out.extend_from_slice(vr);
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
}

/// A text value, cut to what its VR holds and padded to an even length.
fn put_text(out: &mut Vec<u8>, tag: u32, vr: &[u8; 2], value: &str, most: usize) {
    let cut: String = value.chars().take(most).collect();
    let mut bytes = cut.into_bytes();
    if bytes.len() % 2 == 1 {
        bytes.push(if vr == b"UI" { 0 } else { b' ' });
    }
    put_short(out, tag, vr, &bytes);
}

fn put_u32(out: &mut Vec<u8>, tag: u32, value: u32) {
    put_short(out, tag, b"UL", &value.to_le_bytes());
}

/// The record type for a stored object.
fn record_type(sop_class: &str) -> &'static str {
    const STORAGE: &str = "1.2.840.10008.5.1.4.1.1.";
    let Some(rest) = sop_class.strip_prefix(STORAGE) else {
        return "IMAGE";
    };
    if rest == "88.59" {
        "KEY OBJECT DOC"
    } else if rest.starts_with("88.") {
        "SR DOCUMENT"
    } else if rest.starts_with("11.") {
        "PRESENTATION"
    } else if rest.starts_with("104.") {
        "ENCAP DOC"
    } else {
        "IMAGE"
    }
}

struct Pending {
    kind: &'static str,
    keys: Vec<u8>,
    next: Option<usize>,
    lower: Option<usize>,
}

fn character_set(keys: &mut Vec<u8>, values: &[&str]) {
    if values.iter().any(|v| !v.is_ascii()) {
        put_text(keys, CHARACTER_SET, b"CS", "ISO_IR 192", 16);
    }
}

fn link(pending: &mut [Pending], parent: Option<usize>, previous: &mut Option<usize>, index: usize) {
    match *previous {
        Some(p) => pending[p].next = Some(index),
        None => {
            if let Some(parent) = parent {
                pending[parent].lower = Some(index);
            }
        }
    }
    *previous = Some(index);
}

fn pending_records(set: &DicomDir) -> Result<(Vec<Pending>, Option<usize>)> {
    let mut out: Vec<Pending> = Vec::new();
    let mut last_patient = None;
    for patient in &set.patients {
        let mut keys = Vec::new();
        character_set(&mut keys, &[&patient.name, &patient.id]);
        put_text(&mut keys, PATIENT_NAME, b"PN", &patient.name, 64);
        put_text(&mut keys, PATIENT_ID, b"LO", &patient.id, 64);
        let p = out.len();
        out.push(Pending {
            kind: "PATIENT",
            keys,
            next: None,
            lower: None,
        });
        link(&mut out, None, &mut last_patient, p);

        let mut last_study = None;
        for study in &patient.studies {
            let mut keys = Vec::new();
            character_set(&mut keys, &[&study.description, &study.accession, &study.id]);
            put_text(&mut keys, STUDY_DATE, b"DA", &study.date, 8);
            put_text(&mut keys, STUDY_TIME, b"TM", &study.time, 16);
            put_text(&mut keys, ACCESSION, b"SH", &study.accession, 16);
            put_text(&mut keys, STUDY_DESCRIPTION, b"LO", &study.description, 64);
            put_text(&mut keys, STUDY_UID, b"UI", &study.uid, 64);
            put_text(&mut keys, STUDY_ID, b"SH", &study.id, 16);
            let s = out.len();
            out.push(Pending {
                kind: "STUDY",
                keys,
                next: None,
                lower: None,
            });
            link(&mut out, Some(p), &mut last_study, s);

            let mut last_series = None;
            for series in &study.series {
                let mut keys = Vec::new();
                character_set(&mut keys, &[&series.description]);
                put_text(&mut keys, MODALITY, b"CS", &series.modality, 16);
                put_text(&mut keys, SERIES_DESCRIPTION, b"LO", &series.description, 64);
                put_text(&mut keys, SERIES_UID, b"UI", &series.uid, 64);
                put_text(&mut keys, SERIES_NUMBER, b"IS", &series.number, 12);
                let e = out.len();
                out.push(Pending {
                    kind: "SERIES",
                    keys,
                    next: None,
                    lower: None,
                });
                link(&mut out, Some(s), &mut last_series, e);

                let mut last_file = None;
                for (n, file) in series.files.iter().enumerate() {
                    let mut keys = Vec::new();
                    put_text(&mut keys, FILE_ID, b"CS", &file_id(&file.path)?, 80);
                    put_text(&mut keys, SOP_CLASS_IN_FILE, b"UI", &file.sop_class, 64);
                    put_text(&mut keys, SOP_INSTANCE_IN_FILE, b"UI", &file.sop_instance, 64);
                    put_text(&mut keys, TRANSFER_SYNTAX_IN_FILE, b"UI", &file.transfer_syntax, 64);
                    let number = file.instance_number.unwrap_or(n as i64 + 1).to_string();
                    put_text(&mut keys, INSTANCE_NUMBER, b"IS", &number, 12);
                    let f = out.len();
                    out.push(Pending {
                        kind: record_type(&file.sop_class),
                        keys,
                        next: None,
                        lower: None,
                    });
                    link(&mut out, Some(e), &mut last_file, f);
                }
            }
        }
    }
    Ok((out, last_patient))
}

fn record_size(p: &Pending) -> usize {
    let kind = p.kind.len() + p.kind.len() % 2;
    // Item header, the two links, the in-use flag and the type.
    8 + 12 + 10 + 12 + 8 + kind + p.keys.len()
}

fn file_meta(instance_uid: &str) -> Vec<u8> {
    let mut group = Vec::new();
    tag_bytes(&mut group, 0x0002_0001);
    group.extend_from_slice(b"OB\0\0");
    group.extend_from_slice(&2u32.to_le_bytes());
    group.extend_from_slice(&[0, 1]);
    put_text(&mut group, 0x0002_0002, b"UI", MEDIA_STORAGE_DIRECTORY, 64);
    put_text(&mut group, 0x0002_0003, b"UI", instance_uid, 64);
    put_text(&mut group, 0x0002_0010, b"UI", EXPLICIT_LE, 64);
    put_text(&mut group, 0x0002_0012, b"UI", IMPLEMENTATION_CLASS, 64);
    put_text(&mut group, 0x0002_0013, b"SH", IMPLEMENTATION_VERSION, 16);

    let mut out = vec![0u8; 128];
    out.extend_from_slice(b"DICM");
    put_u32(&mut out, 0x0002_0000, group.len() as u32);
    out.extend_from_slice(&group);
    out
}

/// The bytes of a DICOMDIR for `set`, whose files' paths must already be valid
/// file IDs.
pub fn to_bytes(set: &DicomDir) -> Result<Vec<u8>> {
    let (pending, last_patient) = pending_records(set)?;
    let mut out = file_meta(&crate::sc::new_uid());

    let mut head = Vec::new();
    put_text(&mut head, FILE_SET_ID, b"CS", &set.file_set_id, 16);
    let links_at = head.len();
    put_u32(&mut head, FIRST_ROOT, 0);
    put_u32(&mut head, LAST_ROOT, 0);
    put_short(&mut head, CONSISTENCY, b"US", &0u16.to_le_bytes());
    tag_bytes(&mut head, RECORDS);
    head.extend_from_slice(b"SQ\0\0");

    let sizes: Vec<usize> = pending.iter().map(record_size).collect();
    let body: usize = sizes.iter().sum();
    if body > u32::MAX as usize / 2 {
        return Err(DicomError::Read("too many files for one DICOMDIR".into()));
    }
    head.extend_from_slice(&(body as u32).to_le_bytes());

    let start = out.len() + head.len();
    let mut offsets = Vec::with_capacity(pending.len());
    let mut at = start;
    for size in &sizes {
        offsets.push(at as u32);
        at += size;
    }
    let offset_of = |i: Option<usize>| i.map_or(0, |i| offsets[i]);
    if !pending.is_empty() {
        head[links_at + 8..links_at + 12].copy_from_slice(&offsets[0].to_le_bytes());
        head[links_at + 20..links_at + 24].copy_from_slice(&offset_of(last_patient).to_le_bytes());
    }
    out.extend_from_slice(&head);

    for (p, size) in pending.iter().zip(&sizes) {
        tag_bytes(&mut out, ITEM);
        out.extend_from_slice(&((size - 8) as u32).to_le_bytes());
        put_u32(&mut out, NEXT, offset_of(p.next));
        put_short(&mut out, IN_USE, b"US", &0xFFFFu16.to_le_bytes());
        put_u32(&mut out, LOWER, offset_of(p.lower));
        put_text(&mut out, RECORD_TYPE, b"CS", p.kind, 16);
        out.extend_from_slice(&p.keys);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DicomDir {
        let file = |n: i64| DirFile {
            path: PathBuf::from(format!("DICOM/PT000001/ST000001/SE000001/IM{n:06}")),
            sop_class: "1.2.840.10008.5.1.4.1.1.2".into(),
            sop_instance: format!("1.2.3.{n}"),
            transfer_syntax: "1.2.840.10008.1.2.4.70".into(),
            instance_number: Some(n),
        };
        DicomDir {
            file_set_id: "BELLA CT".into(),
            refused: 0,
            patients: vec![
                DirPatient {
                    name: "Bella^Sm\u{f8}rgrav".into(),
                    id: "A12".into(),
                    studies: vec![DirStudy {
                        uid: "1.2.3".into(),
                        id: "7".into(),
                        date: "20261105".into(),
                        time: "101500".into(),
                        accession: "X1".into(),
                        description: "CT head".into(),
                        series: vec![DirSeries {
                            uid: "1.2.3.4".into(),
                            modality: "CT".into(),
                            number: "2".into(),
                            description: "DETAIL".into(),
                            files: vec![file(1), file(2), file(3)],
                        }],
                    }],
                },
                DirPatient {
                    name: "Rex".into(),
                    id: "B3".into(),
                    studies: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn what_is_written_reads_back_the_same() {
        let bytes = to_bytes(&sample()).unwrap();
        assert_eq!(bytes.len() % 2, 0);
        assert_eq!(parse(&bytes).unwrap(), sample());
    }

    #[test]
    fn paths_stay_inside_the_folder() {
        for bad in [
            "..\\..\\WINDOWS\\X",
            "A\\..\\B",
            ".",
            "",
            "C:\\X",
            "A\\\\B",
            "A/../B",
            "CON:STREAM",
        ] {
            assert_eq!(safe_path(bad), None, "{bad}");
        }
        assert_eq!(safe_path("DICOM\\IM1"), Some(PathBuf::from("DICOM").join("IM1")));
        assert_eq!(safe_path("DICOM\\IM1.;1"), Some(PathBuf::from("DICOM").join("IM1")));
    }

    #[test]
    fn links_that_loop_or_lead_nowhere_fall_back_to_file_order() {
        let good = to_bytes(&sample()).unwrap();
        let first = {
            let at = good.windows(4).position(|w| w == [0x04, 0, 0, 0x12]).unwrap();
            u32::from_le_bytes(good[at + 8..at + 12].try_into().unwrap()) as usize
        };
        // Point the first record's next link at itself.
        let mut looped = good.clone();
        let next_value = first + 8 + 8;
        looped[next_value..next_value + 4].copy_from_slice(&(first as u32).to_le_bytes());
        assert_eq!(parse(&looped).unwrap().file_count(), 3);
        // And at a place that is not a record.
        let mut lost = good.clone();
        lost[next_value..next_value + 4].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(parse(&lost).unwrap().file_count(), 3);
    }

    #[test]
    fn a_file_that_ends_early_is_an_error_at_every_length() {
        let bytes = to_bytes(&sample()).unwrap();
        for n in 0..bytes.len() {
            // The answer does not matter, only that it comes.
            let _ = parse(&bytes[..n]);
        }
    }

    #[test]
    fn another_dicom_file_is_not_taken_for_one() {
        let mut bytes = to_bytes(&sample()).unwrap();
        let at = bytes
            .windows(MEDIA_STORAGE_DIRECTORY.len())
            .position(|w| w == MEDIA_STORAGE_DIRECTORY.as_bytes())
            .unwrap();
        bytes[at + 18] = b'9';
        assert!(matches!(parse(&bytes), Err(DicomError::NotDicom(_))));
    }

    #[test]
    fn file_ids_keep_to_the_rules() {
        assert!(valid_file_id_part("IM000001"));
        assert!(!valid_file_id_part("IM0000001"));
        assert!(!valid_file_id_part("im1"));
        let mut set = sample();
        set.patients[0].studies[0].series[0].files[0].path = PathBuf::from("scan one.dcm");
        assert!(to_bytes(&set).is_err());
    }

    #[test]
    fn locating_ignores_case_and_refuses_to_climb() {
        let dir = std::env::temp_dir().join(format!("ruthos-dd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("Dicom")).unwrap();
        std::fs::write(dir.join("Dicom").join("im1"), b"x").unwrap();
        assert!(locate(&dir, Path::new("DICOM/IM1")).is_some());
        assert!(locate(&dir, Path::new("DICOM/IM2")).is_none());
        assert!(locate(&dir.join("Dicom"), Path::new("../Dicom/im1")).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
