//! A package: one patient's images in a folder anyone can open.
//!
//! The folder is laid out for the person receiving it. Its DICOM files and
//! DICOMDIR follow the standard, so any viewer or PACS can read it. The Ruthos
//! program goes in unchanged, renamed so that it says what it does, and a
//! `ruthos.ini` beside it makes it open those images and nothing else. The
//! program is never altered per patient: Windows and antivirus software judge
//! an executable by its contents, and an altered one would be unknown to them
//! every time. A report and plain pictures sit alongside, as the person may be
//! on a phone or a Mac, or on a computer that refuses to run programs from a
//! stick.
//!
//! Measurements live in the archive and not in the DICOM files, so they travel
//! in a small notes file, keyed by SOP Instance UID.
//!
//! The work happens in a `.partial` folder, which takes its real name only
//! after every file has been read back. A package that exists is complete.

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::tags;
use serde::{Deserialize, Serialize};

use ruthos_dicom::dicomdir::{self, DicomDir, DirFile, DirPatient, DirSeries, DirStudy};

use crate::db::Database;
use crate::paths::DataPaths;
use crate::report::{write_pdf, ReportDocument};
use crate::teaching::{SidecarCalibration, SidecarMeasurement};
use crate::{CoreError, ImageRow, Result, SeriesRow, StudyRow};

/// The program in a package. Windows hides the extension, so people read
/// "Ruthos Viewer". It is named as a program, not as an action on the files
/// beside it: an executable on a stick named like a command to open them is
/// the shape of USB malware, which antivirus heuristics flag and practice
/// staff are trained not to run.
pub const PROGRAM_NAME: &str = "Ruthos Viewer.exe";
pub const NOTES_NAME: &str = "ruthos-package.json";
pub const READ_ME_NAME: &str = "Read me first.html";
pub const KEY_IMAGES_DIR: &str = "Key images";
pub const LICENCES_DIR: &str = "Licences";
const CHUNK: usize = 1 << 20;

const INI: &str = "; Makes the program beside this file open the images in this folder,\r\n\
    ; without touching any Ruthos already on this computer.\r\n\
    [viewer]\r\n\
    open = DICOMDIR\r\n";

/// A picture for people without a DICOM viewer.
#[derive(Debug, Clone)]
pub struct KeyImage {
    pub caption: String,
    pub jpeg: Vec<u8>,
}

/// What goes in a package.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub studies: Vec<i64>,
    /// The folder's name, in words: "Bella CT 5 Nov 2026".
    pub title: String,
    /// The Ruthos program to include. `None` writes plain DICOM.
    pub program: Option<PathBuf>,
    /// Files that must travel with the program.
    pub licences: Vec<PathBuf>,
    /// Take the owner's name and contact details out of the copies.
    pub remove_owner: bool,
    /// File name (without extension) and contents of each report.
    pub reports: Vec<(String, ReportDocument)>,
    pub key_images: Vec<KeyImage>,
}

/// How big a package will be.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Size {
    pub files: usize,
    pub bytes: u64,
    /// Files whose copy would not fit on a FAT32 stick.
    pub over_4gb: usize,
}

const FAT32_LIMIT: u64 = (1 << 32) - 1;

/// Measurements and marks that live in the archive.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    pub format: u32,
    pub measurements: Vec<SidecarMeasurement>,
    pub calibrations: Vec<SidecarCalibration>,
    pub key_images: Vec<String>,
}

/// Told how far a package has got: files done, and files in all.
pub type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

fn cancelled() -> CoreError {
    CoreError::Other("cancelled".into())
}

/// A name Windows, macOS and a FAT stick all accept.
pub fn folder_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(64).collect();
    let name = cut.trim().trim_end_matches('.').trim().to_string();
    if name.is_empty() {
        "Images".to_string()
    } else {
        name
    }
}

/// The animal's name from a Patient Name, which in veterinary use often has
/// the owner's surname as its family name: `SMITH^Bella` becomes `Bella`.
pub fn name_without_owner(patient_name: &str) -> String {
    let first_group = patient_name.split('=').next().unwrap_or("");
    let mut parts = first_group.split('^');
    let family = parts.next().unwrap_or("").trim();
    match parts.next().map(str::trim).filter(|g| !g.is_empty()) {
        Some(given) => given.to_string(),
        None => family.to_string(),
    }
}

fn dicom_date(iso: Option<&str>) -> String {
    iso.unwrap_or("").replace('-', "")
}

fn dicom_time(iso: Option<&str>) -> String {
    iso.unwrap_or("").replace(':', "")
}

type StudyImages = Vec<(SeriesRow, Vec<ImageRow>)>;

fn images_of(db: &Database, study: i64) -> Result<StudyImages> {
    let mut out = Vec::new();
    for series in db.series_for_study(study)? {
        let images = db.images_for_series(series.id)?;
        if !images.is_empty() {
            out.push((series, images));
        }
    }
    Ok(out)
}

/// The size of the images in these studies, before the program and pictures.
pub fn size_of(db: &Database, studies: &[i64]) -> Result<Size> {
    let mut size = Size::default();
    for &study in studies {
        for (_, images) in images_of(db, study)? {
            for image in images {
                let bytes = image.file_size.max(0) as u64;
                size.files += 1;
                size.bytes += bytes;
                if bytes > FAT32_LIMIT {
                    size.over_4gb += 1;
                }
            }
        }
    }
    Ok(size)
}

/// Read a file back and compare it with its source.
fn matches_source(source: &Path, copy: &Path) -> std::io::Result<bool> {
    let mut a = BufReader::with_capacity(CHUNK, std::fs::File::open(source)?);
    let mut b = BufReader::with_capacity(CHUNK, std::fs::File::open(copy)?);
    let mut left = vec![0u8; CHUNK];
    let mut right = vec![0u8; CHUNK];
    loop {
        let n = a.read(&mut left)?;
        if n == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
        b.read_exact(&mut right[..n])?;
        if left[..n] != right[..n] {
            return Ok(false);
        }
    }
}

fn is_part_10(path: &Path) -> std::io::Result<bool> {
    let mut head = [0u8; 132];
    let mut file = std::fs::File::open(path)?;
    match file.read_exact(&mut head) {
        Ok(()) => Ok(&head[128..] == b"DICM"),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// Copy a file exactly and prove it.
fn copy_verified(source: &Path, copy: &Path) -> Result<()> {
    let mut from = std::fs::File::open(source)?;
    let mut to = std::fs::File::create(copy)?;
    std::io::copy(&mut from, &mut to)?;
    to.flush()?;
    to.sync_all()?;
    drop(to);
    if !matches_source(source, copy)? {
        return Err(CoreError::Other(format!(
            "{} did not read back the same. The disk may be failing.",
            copy.display()
        )));
    }
    Ok(())
}

/// Copy a DICOM file with its meta header, and without the owner when asked.
/// The pixel data is carried over as it is. Its UIDs stay, so the copy is
/// still recognised as the same image by anything that has seen it.
fn copy_rewritten(source: &Path, copy: &Path, sop_uid: &str, remove_owner: bool) -> Result<()> {
    let mut object = ruthos_dicom::open::open_any(source, None)?;
    if remove_owner {
        for tag in [
            tags::RESPONSIBLE_PERSON,
            tags::RESPONSIBLE_PERSON_ROLE,
            tags::RESPONSIBLE_ORGANIZATION,
            tags::PATIENT_ADDRESS,
            tags::PATIENT_TELEPHONE_NUMBERS,
            tags::OTHER_PATIENT_NAMES,
            tags::PATIENT_BIRTH_NAME,
            tags::PATIENT_MOTHER_BIRTH_NAME,
        ] {
            object.remove_element(tag);
        }
        let name = object
            .element(tags::PATIENT_NAME)
            .ok()
            .and_then(|e| e.to_str().ok().map(|s| s.to_string()));
        if let Some(name) = name {
            object.put(DataElement::new(
                tags::PATIENT_NAME,
                VR::PN,
                PrimitiveValue::from(name_without_owner(&name)),
            ));
        }
    }
    object
        .write_to_file(copy)
        .map_err(|e| CoreError::Other(format!("{}: {e}", copy.display())))?;
    // Windows only flushes a file opened for writing.
    std::fs::OpenOptions::new().write(true).open(copy)?.sync_all()?;
    let written = ruthos_dicom::read_meta(copy)?;
    if written.sop_instance_uid != sop_uid {
        return Err(CoreError::Other(format!("{} did not read back", copy.display())));
    }
    Ok(())
}

struct PlannedPatient {
    name: String,
    id: String,
    studies: Vec<(StudyRow, StudyImages)>,
}

struct Plan {
    patients: Vec<PlannedPatient>,
    total: usize,
}

fn plan(db: &Database, studies: &[i64]) -> Result<Plan> {
    let mut grouped: BTreeMap<(String, String), Vec<(StudyRow, StudyImages)>> = BTreeMap::new();
    let mut total = 0;
    for &id in studies {
        let study = db
            .study(id)?
            .ok_or_else(|| CoreError::Other("that study is no longer in the archive".into()))?;
        let series = images_of(db, id)?;
        total += series.iter().map(|(_, images)| images.len()).sum::<usize>();
        grouped
            .entry((study.patient_name.clone(), study.patient_id.clone()))
            .or_default()
            .push((study, series));
    }
    if total == 0 {
        return Err(CoreError::Other("those studies have no images".into()));
    }
    let patients = grouped
        .into_iter()
        .map(|((name, id), studies)| PlannedPatient { name, id, studies })
        .collect();
    Ok(Plan { patients, total })
}

fn collect_notes(db: &Database, notes: &mut Notes, image: &ImageRow) -> Result<()> {
    let uid = &image.sop_instance_uid;
    for frame in 0..image.number_of_frames.max(1) {
        for r in db.rois(image.id, frame)? {
            notes.measurements.push(SidecarMeasurement {
                sop_instance_uid: uid.clone(),
                frame,
                kind: r.kind,
                points: r.points,
                name: r.name,
                value: r.value,
                unit: r.unit,
                basis: r.basis,
                def_key: r.def_key,
            });
        }
    }
    if let Some([row_mm, col_mm]) = db.calibration(image.id)? {
        notes.calibrations.push(SidecarCalibration {
            sop_instance_uid: uid.clone(),
            row_mm,
            col_mm,
        });
    }
    if image.is_key_image {
        notes.key_images.push(uid.clone());
    }
    Ok(())
}

/// Everything a package write needs to hand around.
struct Job<'a> {
    db: &'a Database,
    paths: &'a DataPaths,
    spec: &'a Spec,
    progress: Progress<'a>,
    cancel: &'a AtomicBool,
}

impl Job<'_> {
    fn copy_image(&self, image: &ImageRow, row: &SeriesRow, number: usize, copy: &Path) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let source = self.paths.resolve(&image.path);
        if !source.is_file() {
            return Err(CoreError::Other(format!(
                "Image {number} of {} cannot be found at {}. Nothing was exported.",
                row.display_name(),
                source.display()
            )));
        }
        if !self.spec.remove_owner && is_part_10(&source)? {
            copy_verified(&source, copy)
        } else {
            copy_rewritten(&source, copy, &image.sop_instance_uid, self.spec.remove_owner)
        }
    }

    /// Copy every image into `work`, returning the index of what was written.
    fn write_images(&self, work: &Path, plan: &Plan, notes: &mut Notes) -> Result<DicomDir> {
        let mut index = DicomDir {
            file_set_id: file_set_id(&self.spec.title),
            ..DicomDir::default()
        };
        let mut done = 0;
        for (p, planned) in plan.patients.iter().enumerate() {
            let mut patient = DirPatient {
                name: if self.spec.remove_owner {
                    name_without_owner(&planned.name)
                } else {
                    planned.name.clone()
                },
                id: planned.id.clone(),
                studies: Vec::new(),
            };
            let patient_dir = PathBuf::from("DICOM").join(format!("PT{:06}", p + 1));
            for (s, (study, series)) in planned.studies.iter().enumerate() {
                let mut entry = DirStudy {
                    uid: study.study_instance_uid.clone(),
                    id: (s + 1).to_string(),
                    date: dicom_date(study.study_date.as_deref()),
                    time: dicom_time(study.study_time.as_deref()),
                    accession: study.accession_number.clone(),
                    description: study.study_description.clone(),
                    series: Vec::new(),
                };
                let study_dir = patient_dir.join(format!("ST{:06}", s + 1));
                for (e, (row, images)) in series.iter().enumerate() {
                    let series_dir = study_dir.join(format!("SE{:06}", e + 1));
                    std::fs::create_dir_all(work.join(&series_dir))?;
                    let mut series_entry = DirSeries {
                        uid: row.series_instance_uid.clone(),
                        modality: row.modality.clone(),
                        number: row.series_number.unwrap_or(e as i64 + 1).to_string(),
                        description: row.series_description.clone(),
                        files: Vec::with_capacity(images.len()),
                    };
                    for (i, image) in images.iter().enumerate() {
                        let file_id = series_dir.join(format!("IM{:06}", i + 1));
                        self.copy_image(image, row, i + 1, &work.join(&file_id))?;
                        collect_notes(self.db, notes, image)?;
                        series_entry.files.push(DirFile {
                            path: file_id,
                            sop_class: image.sop_class_uid.clone(),
                            sop_instance: image.sop_instance_uid.clone(),
                            transfer_syntax: image.transfer_syntax.clone(),
                            instance_number: image.instance_number,
                        });
                        done += 1;
                        (self.progress)(done, plan.total);
                    }
                    entry.series.push(series_entry);
                }
                patient.studies.push(entry);
            }
            index.patients.push(patient);
        }
        Ok(index)
    }

    fn build(&self, work: &Path) -> Result<()> {
        let plan = plan(self.db, &self.spec.studies)?;
        std::fs::create_dir_all(work)?;
        let mut notes = Notes {
            format: 1,
            ..Notes::default()
        };
        let index = self.write_images(work, &plan, &mut notes)?;
        write_extras(self.spec, &plan, work, &notes)?;
        // Last, so that a folder with a DICOMDIR has all of its files.
        let bytes = dicomdir::to_bytes(&index)?;
        let path = work.join(dicomdir::FILE_NAME);
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        let back = dicomdir::open(&path)?;
        let (_, missing) = back.locate_all(work);
        if back.file_count() != plan.total || missing != 0 {
            return Err(CoreError::Other("the finished package did not check out".into()));
        }
        Ok(())
    }
}

fn file_set_id(title: &str) -> String {
    title
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(16)
        .collect()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn link(path: &str) -> String {
    path.replace('%', "%25").replace(' ', "%20").replace('#', "%23")
}

/// The page that works on anything, with or without Ruthos.
fn read_me(spec: &Spec, plan: &Plan, pictures: &[String], reports: &[String]) -> String {
    let mut studies = String::new();
    for planned in &plan.patients {
        let shown = if spec.remove_owner {
            name_without_owner(&planned.name)
        } else {
            planned.name.replace('^', " ")
        };
        studies.push_str(&format!("<h1>{}</h1>\n<ul>\n", escape(shown.trim())));
        for (study, series) in &planned.studies {
            let date = study.study_date.as_deref().unwrap_or("Date not recorded");
            let what = if study.study_description.is_empty() {
                study.modalities.replace('\\', ", ")
            } else {
                study.study_description.clone()
            };
            let images: usize = series.iter().map(|(_, i)| i.len()).sum();
            studies.push_str(&format!(
                "<li>{}: {} ({images} image{})</li>\n",
                escape(date),
                escape(&what),
                if images == 1 { "" } else { "s" }
            ));
        }
        studies.push_str("</ul>\n");
    }
    let mut documents = String::new();
    for report in reports {
        documents.push_str(&format!(
            "<p><a class=\"report\" href=\"{}\">Read the report</a></p>\n",
            link(report)
        ));
    }
    let mut gallery = String::new();
    if !pictures.is_empty() {
        gallery.push_str("<h2>Key images</h2>\n");
        for (file, image) in pictures.iter().zip(&spec.key_images) {
            gallery.push_str(&format!(
                "<figure><img src=\"{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>\n",
                link(file),
                escape(&image.caption),
                escape(&image.caption)
            ));
        }
    }
    let open = if spec.program.is_some() {
        "<h2>See every image</h2>\n\
         <p><b>On a Windows computer:</b> double-click <b>Ruthos Viewer</b> in this folder. You don't need to install \
         anything, and it changes nothing on your computer.</p>\n\
         <p>If this came as a zip file, choose <b>Extract All</b> first and open the extracted folder.</p>\n\
         <p>If Windows says it protected your PC, choose <b>More info</b>, then <b>Run anyway</b>. If your computer \
         won't allow that, the report and pictures on this page are still yours to read.</p>\n\
         <p><b>On a Mac, phone or tablet:</b> you can read this page, but not open the full set of images. \
         Your vet can read the DICOM folder with any DICOM viewer.</p>\n"
    } else {
        "<h2>See every image</h2>\n<p>Open the DICOMDIR file in this folder with any DICOM viewer.</p>\n"
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title>\n<style>\n\
         body{{font:17px/1.5 system-ui,sans-serif;max-width:46rem;margin:0 auto;padding:1.5rem 1rem;color:#1b1f24;background:#fff}}\n\
         h1{{font-size:1.7rem;margin:0 0 .4rem}}h2{{font-size:1.2rem;margin-top:2rem}}\n\
         ul{{padding-left:1.2rem}}img{{max-width:100%;height:auto;background:#000}}\n\
         figure{{margin:0 0 1.2rem}}figcaption{{font-size:.9rem;color:#555}}\n\
         a.report{{display:inline-block;padding:.6rem 1rem;background:#0b5cad;color:#fff;border-radius:6px;text-decoration:none}}\n\
         </style></head>\n<body>\n{studies}{documents}{gallery}{open}</body></html>\n",
        escape(&spec.title)
    )
}

fn write_extras(spec: &Spec, plan: &Plan, work: &Path, notes: &Notes) -> Result<()> {
    let mut reports = Vec::new();
    for (name, document) in &spec.reports {
        let file = format!("{}.pdf", folder_name(name));
        write_pdf(document, &work.join(&file))?;
        reports.push(file);
    }
    let mut pictures = Vec::new();
    if !spec.key_images.is_empty() {
        std::fs::create_dir_all(work.join(KEY_IMAGES_DIR))?;
        for (i, image) in spec.key_images.iter().enumerate() {
            let file = format!("{KEY_IMAGES_DIR}/{:03}.jpg", i + 1);
            std::fs::write(work.join(&file), &image.jpeg)?;
            pictures.push(file);
        }
    }
    std::fs::write(work.join(READ_ME_NAME), read_me(spec, plan, &pictures, &reports))?;
    let json = serde_json::to_string_pretty(notes).map_err(|e| CoreError::Other(e.to_string()))?;
    std::fs::write(work.join(NOTES_NAME), json)?;

    if let Some(program) = &spec.program {
        copy_verified(program, &work.join(PROGRAM_NAME))?;
        std::fs::write(work.join("ruthos.ini"), INI)?;
        let licences = work.join(LICENCES_DIR);
        std::fs::create_dir_all(&licences)?;
        std::fs::write(
            licences.join("About this program.txt"),
            "\"Ruthos Viewer\" is Ruthos, a free and open-source DICOM viewer, under another file name.\r\n\
             Source code: https://github.com/Ruthos/Ruthos64\r\n",
        )?;
        for file in &spec.licences {
            if let Some(name) = file.file_name() {
                std::fs::copy(file, licences.join(name))?;
            }
        }
    }
    Ok(())
}

/// A name beside others that does not collide with them.
fn free_name(parent: &Path, name: &str, extension: &str) -> PathBuf {
    let mut candidate = parent.join(format!("{name}{extension}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{name} ({n}){extension}"));
        n += 1;
    }
    candidate
}

/// Write a package as a folder inside `parent`, returning where it went.
pub fn write_folder(
    db: &Database,
    paths: &DataPaths,
    spec: &Spec,
    parent: &Path,
    progress: Progress,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let job = Job {
        db,
        paths,
        spec,
        progress,
        cancel,
    };
    let target = free_name(parent, &folder_name(&spec.title), "");
    let work = target.with_file_name(format!(
        "{}.partial",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&work);
    let result = job.build(&work).and_then(|()| {
        std::fs::rename(&work, &target)?;
        Ok(())
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }
    Ok(target)
}

fn add_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    from: &Path,
    name: &str,
    options: zip::write::SimpleFileOptions,
    cancel: &AtomicBool,
) -> Result<()> {
    let fail = |e: zip::result::ZipError| CoreError::Other(format!("could not write the zip: {e}"));
    if from.is_dir() {
        zip.add_directory(format!("{name}/"), options).map_err(fail)?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(from)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let leaf = entry.file_name().unwrap_or_default().to_string_lossy().to_string();
            add_to_zip(zip, &entry, &format!("{name}/{leaf}"), options, cancel)?;
        }
    } else {
        zip.start_file(name, options).map_err(fail)?;
        std::io::copy(&mut std::fs::File::open(from)?, zip)?;
    }
    Ok(())
}

/// Write a package as a zip file, `path`, holding one folder.
pub fn write_zip(
    db: &Database,
    paths: &DataPaths,
    spec: &Spec,
    path: &Path,
    progress: Progress,
    cancel: &AtomicBool,
) -> Result<()> {
    let job = Job {
        db,
        paths,
        spec,
        progress,
        cancel,
    };
    let scratch = paths.temp_dir().join(format!("package-{}", uuid::Uuid::new_v4()));
    let name = folder_name(&spec.title);
    let unfinished = path.with_extension("zip.partial");
    let result = (|| {
        let folder = scratch.join(&name);
        job.build(&folder)?;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&unfinished)?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        add_to_zip(&mut zip, &folder, &name, options, cancel)?;
        let file = zip
            .finish()
            .map_err(|e| CoreError::Other(format!("could not write the zip: {e}")))?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&unfinished, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    if result.is_err() {
        let _ = std::fs::remove_file(&unfinished);
    }
    result
}

/// Put a package's measurements and marks on the images just read from it.
pub fn apply_notes(db: &Database, folder: &Path) -> Result<()> {
    let path = folder.join(NOTES_NAME);
    if !path.is_file() {
        return Ok(());
    }
    let notes: Notes =
        serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| CoreError::Other(e.to_string()))?;
    let mut by_image: BTreeMap<(i64, i64), Vec<crate::RoiRecord>> = BTreeMap::new();
    for m in notes.measurements {
        if let Some(image) = db.image_by_sop_uid(&m.sop_instance_uid)? {
            by_image.entry((image.id, m.frame)).or_default().push(crate::RoiRecord {
                id: 0,
                image_id: image.id,
                frame: m.frame,
                kind: m.kind,
                points: m.points,
                name: m.name,
                value: m.value,
                unit: m.unit,
                basis: m.basis,
                def_key: m.def_key,
            });
        }
    }
    for ((image, frame), rois) in by_image {
        db.replace_rois(image, frame, &rois)?;
    }
    for c in notes.calibrations {
        if let Some(image) = db.image_by_sop_uid(&c.sop_instance_uid)? {
            db.set_calibration(image.id, c.row_mm, c.col_mm, "package")?;
        }
    }
    for uid in notes.key_images {
        if let Some(image) = db.image_by_sop_uid(&uid)? {
            db.set_key_image(image.id, true)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_suit_any_disk() {
        assert_eq!(folder_name("Bella CT 5 Nov 2026"), "Bella CT 5 Nov 2026");
        assert_eq!(folder_name("A/B: C?"), "A B C");
        assert_eq!(folder_name("..."), "Images");
        assert_eq!(folder_name("trailing dot."), "trailing dot");
    }

    #[test]
    fn the_owner_comes_off_the_patient_name() {
        assert_eq!(name_without_owner("SMITH^Bella"), "Bella");
        assert_eq!(name_without_owner("Bella"), "Bella");
        assert_eq!(name_without_owner("SMITH^Bella=x"), "Bella");
    }

    #[test]
    fn the_page_cannot_be_broken_by_a_name() {
        assert_eq!(escape("<b>&\""), "&lt;b&gt;&amp;&quot;");
        assert_eq!(link("Key images/001.jpg"), "Key%20images/001.jpg");
    }
}
