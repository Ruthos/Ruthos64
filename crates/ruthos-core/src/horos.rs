//! Moving from Horos: read a Horos (or OsiriX) database folder and bring its
//! studies into Ruthos with their albums, comments, status and key images.
//!
//! A Horos library is a folder called `Horos Data` holding `Database.sql`
//! (a Core Data SQLite store) and `DATABASE.noindex`, where the images live
//! as numbered files in buckets of ten thousand. The store is never opened
//! in place: it is copied, with its write-ahead log, to Ruthos's temporary
//! folder and read from there, so Horos's own files are not touched.
//!
//! The images are imported through the normal import path, so they are
//! filed, de-duplicated and indexed exactly as any other import. What only
//! the Horos database knows (comments, status, key images, the date a study
//! was added, albums) is then laid over the imported studies. Ruthos's own
//! values win: a comment already written in Ruthos is not replaced.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};

use crate::db::{Database, StudyFilter};
use crate::import::{import_file, FileOutcome, ImportMode};
use crate::model::DateFilter;
use crate::paths::DataPaths;
use crate::{CoreError, Result};

/// Seconds between the Unix epoch and Core Data's (1 January 2001).
const CORE_DATA_EPOCH: f64 = 978_307_200.0;

/// Series Horos writes for its own bookkeeping (measurements, reports,
/// annotations as structured reports). They are not images.
const HOROS_PRIVATE_SERIES: [&str; 4] = [
    "OsiriX ROI SR",
    "OsiriX Report SR",
    "OsiriX Annotations SR",
    "OsiriX No Autodeletion",
];

/// Horos's built-in smart albums, which Ruthos has too.
const BUILT_IN_ALBUMS: [&str; 4] = ["Just Added", "Just Opened", "Cases with comments", "Today"];

/// Where a Horos library is.
#[derive(Debug, Clone, PartialEq)]
pub struct HorosLibrary {
    /// The `Horos Data` (or `OsiriX Data`) folder.
    pub data_dir: PathBuf,
    /// `Database.sql` inside it.
    pub database: PathBuf,
}

/// Find a Horos library from a folder the user picked: the `Horos Data`
/// folder itself, the folder that holds it, its `DATABASE.noindex`, or the
/// `Database.sql` file.
pub fn locate(picked: &Path) -> Option<HorosLibrary> {
    let mut candidates = vec![picked.to_path_buf()];
    if let Some(parent) = picked.parent() {
        candidates.push(parent.to_path_buf());
    }
    for name in ["Horos Data", "OsiriX Data", "Horos Data.noindex"] {
        candidates.push(picked.join(name));
    }
    for c in candidates {
        let db = if c.is_file() { c.clone() } else { c.join("Database.sql") };
        if db.is_file() && db.file_name().is_some_and(|n| n == "Database.sql") {
            if let Some(dir) = db.parent() {
                return Some(HorosLibrary {
                    data_dir: dir.to_path_buf(),
                    database: db.clone(),
                });
            }
        }
    }
    None
}

/// A study as Horos recorded it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HorosStudy {
    pub pk: i64,
    pub study_instance_uid: String,
    pub patient_name: String,
    /// Horos's comment fields, non-empty ones joined.
    pub comment: String,
    /// 0 empty, 1 unread, 2 reviewed, 3 dictated, 4 validated.
    pub state: i64,
    /// Unix seconds.
    pub date_added: Option<i64>,
    pub date_opened: Option<i64>,
    pub has_report: bool,
}

/// An image as Horos recorded it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HorosImage {
    pub study_pk: i64,
    /// The file, when it could be found on this computer.
    pub file: Option<PathBuf>,
    /// What Horos recorded as the location, for the report of missing files.
    pub recorded_path: String,
    pub sop_instance_uid: Option<String>,
    pub key_image: bool,
    pub comment: String,
    /// One of Horos's own structured reports (measurements, reports).
    pub horos_private: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HorosAlbum {
    pub name: String,
    pub smart: bool,
    pub predicate: Option<String>,
    pub study_pks: Vec<i64>,
}

/// Everything read from a Horos database.
#[derive(Debug, Clone, Default)]
pub struct HorosCatalog {
    pub library: Option<HorosLibrary>,
    pub studies: Vec<HorosStudy>,
    pub images: Vec<HorosImage>,
    pub albums: Vec<HorosAlbum>,
}

impl HorosCatalog {
    /// Images that will be imported (found on disk, not Horos's own files).
    pub fn importable(&self) -> usize {
        self.images
            .iter()
            .filter(|i| !i.horos_private && i.file.is_some())
            .count()
    }

    /// Images whose file was not found.
    pub fn missing(&self) -> usize {
        self.images
            .iter()
            .filter(|i| !i.horos_private && i.file.is_none())
            .count()
    }

    pub fn key_images(&self) -> usize {
        self.images.iter().filter(|i| i.key_image && !i.horos_private).count()
    }

    pub fn commented_studies(&self) -> usize {
        self.studies.iter().filter(|s| !s.comment.is_empty()).count()
    }

    /// Bytes to copy.
    pub fn bytes(&self) -> u64 {
        self.images
            .iter()
            .filter(|i| !i.horos_private)
            .filter_map(|i| i.file.as_ref())
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum()
    }
}

/// Read a Horos database. The store is copied first, so a Horos that is
/// still running (on a shared drive, say) is not disturbed.
pub fn read_catalog(library: &HorosLibrary, paths: &DataPaths) -> Result<HorosCatalog> {
    let work = paths.temp_dir().join(format!(
        "horos-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&work)?;
    let result = (|| {
        let copy = work.join("Database.sql");
        std::fs::copy(&library.database, &copy)?;
        for suffix in ["-wal", "-shm"] {
            let side = PathBuf::from(format!("{}{suffix}", library.database.display()));
            if side.is_file() {
                std::fs::copy(&side, work.join(format!("Database.sql{suffix}")))?;
            }
        }
        let conn = Connection::open(&copy)?;
        let mut catalog = read_from(&conn, &library.data_dir)?;
        catalog.library = Some(library.clone());
        Ok(catalog)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// The columns a table has, upper case.
fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    Ok(names.flatten().map(|n| n.to_ascii_uppercase()).collect())
}

/// `column` if the table has it, otherwise `NULL`, so a Horos or OsiriX
/// version without the column still reads.
fn col(have: &HashSet<String>, name: &str) -> String {
    if have.contains(name) {
        name.to_string()
    } else {
        "NULL".to_string()
    }
}

fn core_data_date(v: Option<f64>) -> Option<i64> {
    v.filter(|v| v.is_finite() && *v != 0.0)
        .map(|v| (v + CORE_DATA_EPOCH) as i64)
}

/// Horos stores SOP Instance UIDs two characters to a byte: 1–10 are the
/// digits, 11 is the dot, 0 pads the end.
pub fn decode_sop_uid(bytes: &[u8]) -> Option<String> {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        for nibble in [b >> 4, b & 15] {
            match nibble {
                0 => {}
                1..=10 => s.push((b'0' + nibble - 1) as char),
                11 => s.push('.'),
                _ => return None,
            }
        }
    }
    (!s.is_empty()).then_some(s)
}

/// Every file under `DATABASE.noindex`, by file name. Horos files are
/// numbered (`1234.dcm`) and bucketed by a folder size the user could
/// change, so the index finds them whatever the bucket size was.
fn index_database_folder(data_dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    let root = data_dir.join("DATABASE.noindex");
    for e in walkdir::WalkDir::new(&root).into_iter().flatten() {
        if e.file_type().is_file() {
            map.insert(e.file_name().to_string_lossy().to_string(), e.path().to_path_buf());
        }
    }
    map
}

/// Where an image's file is on this computer. Horos keeps files in its
/// database folder as numbered files; other files (imported in place) are
/// recorded by their Mac path, which is tried relative to the library in
/// case the whole folder was copied over.
fn resolve_file(
    data_dir: &Path,
    index: &HashMap<String, PathBuf>,
    path_number: Option<i64>,
    path_string: Option<&str>,
) -> (Option<PathBuf>, String) {
    if let Some(n) = path_number {
        let name = format!("{n}.dcm");
        return (index.get(&name).cloned(), format!("DATABASE.noindex/{name}"));
    }
    let Some(p) = path_string.filter(|p| !p.is_empty()) else {
        return (None, String::new());
    };
    if !p.starts_with('/') {
        return (index.get(p).cloned(), format!("DATABASE.noindex/{p}"));
    }
    // An absolute Mac path. Inside a Horos or OsiriX library: relative to
    // the one we have.
    for marker in ["/Horos Data/", "/OsiriX Data/"] {
        if let Some(i) = p.find(marker) {
            let rest = &p[i + marker.len()..];
            let f = rest.split('/').fold(data_dir.to_path_buf(), |acc, part| acc.join(part));
            if f.is_file() {
                return (Some(f), p.to_string());
            }
        }
    }
    let local = PathBuf::from(p);
    (local.is_file().then_some(local), p.to_string())
}

fn read_from(conn: &Connection, data_dir: &Path) -> Result<HorosCatalog> {
    for t in ["ZSTUDY", "ZSERIES", "ZIMAGE"] {
        let exists: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [t],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(CoreError::Other(
                "this Database.sql is not a Horos or OsiriX database (it has no study table)".into(),
            ));
        }
    }

    // Studies.
    let c = columns(conn, "ZSTUDY")?;
    let sql = format!(
        "SELECT Z_PK, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} FROM ZSTUDY",
        col(&c, "ZSTUDYINSTANCEUID"),
        col(&c, "ZNAME"),
        col(&c, "ZCOMMENT"),
        col(&c, "ZCOMMENT2"),
        col(&c, "ZCOMMENT3"),
        col(&c, "ZCOMMENT4"),
        col(&c, "ZSTATETEXT"),
        col(&c, "ZDATEADDED"),
        col(&c, "ZDATEOPENED"),
        col(&c, "ZREPORTURL"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let studies: Vec<HorosStudy> = stmt
        .query_map([], |r| {
            let comments: Vec<String> = (3..=6)
                .filter_map(|i| r.get::<_, Option<String>>(i).ok().flatten())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            Ok(HorosStudy {
                pk: r.get(0)?,
                study_instance_uid: r.get::<_, Option<String>>(1)?.unwrap_or_default().trim().to_string(),
                patient_name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                comment: comments.join(" · "),
                state: r.get::<_, Option<i64>>(7).ok().flatten().unwrap_or(0).clamp(0, 4),
                date_added: core_data_date(r.get::<_, Option<f64>>(8).ok().flatten()),
                date_opened: core_data_date(r.get::<_, Option<f64>>(9).ok().flatten()),
                has_report: r
                    .get::<_, Option<String>>(10)
                    .ok()
                    .flatten()
                    .is_some_and(|s| !s.trim().is_empty()),
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    // Series: which study each belongs to, and whether it is Horos's own.
    let c = columns(conn, "ZSERIES")?;
    let sql = format!(
        "SELECT Z_PK, {}, {}, {} FROM ZSERIES",
        col(&c, "ZSTUDY"),
        col(&c, "ZNAME"),
        col(&c, "ZMODALITY")
    );
    let mut stmt = conn.prepare(&sql)?;
    let series: HashMap<i64, (i64, bool)> = stmt
        .query_map([], |r| {
            let name: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
            let private = HOROS_PRIVATE_SERIES.iter().any(|p| name.eq_ignore_ascii_case(p));
            Ok((
                r.get::<_, i64>(0)?,
                (r.get::<_, Option<i64>>(1)?.unwrap_or(-1), private),
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    // Images.
    let index = index_database_folder(data_dir);
    let c = columns(conn, "ZIMAGE")?;
    let sql = format!(
        "SELECT {}, {}, {}, {}, {}, {} FROM ZIMAGE",
        col(&c, "ZSERIES"),
        col(&c, "ZPATHNUMBER"),
        col(&c, "ZPATHSTRING"),
        col(&c, "ZCOMPRESSEDSOPINSTANCEUID"),
        col(&c, "ZSTOREDISKEYIMAGE"),
        col(&c, "ZCOMMENT"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let images: Vec<HorosImage> = stmt
        .query_map([], |r| {
            let series_pk: Option<i64> = r.get(0)?;
            let (study_pk, private) = series_pk.and_then(|s| series.get(&s).copied()).unwrap_or((-1, false));
            let path_number: Option<i64> = r.get(1).ok().flatten();
            let path_string: Option<String> = r.get(2).ok().flatten();
            let (file, recorded_path) = resolve_file(data_dir, &index, path_number, path_string.as_deref());
            let uid: Option<Vec<u8>> = r.get(3).ok().flatten();
            Ok(HorosImage {
                study_pk,
                file,
                recorded_path,
                sop_instance_uid: uid.as_deref().and_then(decode_sop_uid),
                key_image: r.get::<_, Option<i64>>(4).ok().flatten().unwrap_or(0) != 0,
                comment: r
                    .get::<_, Option<String>>(5)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                horos_private: private,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    // Albums, and the join table between albums and studies. Core Data
    // names the join table after entity numbers that differ between
    // versions, so it is found by its two columns.
    let mut albums = Vec::new();
    let has_album: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'ZALBUM'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if has_album.is_some() {
        let c = columns(conn, "ZALBUM")?;
        let sql = format!(
            "SELECT Z_PK, {}, {}, {} FROM ZALBUM ORDER BY {}",
            col(&c, "ZNAME"),
            col(&c, "ZSMARTALBUM"),
            col(&c, "ZPREDICATESTRING"),
            if c.contains("ZINDEX") { "ZINDEX" } else { "Z_PK" }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(i64, HorosAlbum)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    HorosAlbum {
                        name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        smart: r.get::<_, Option<i64>>(2).ok().flatten().unwrap_or(0) != 0,
                        predicate: r
                            .get::<_, Option<String>>(3)
                            .ok()
                            .flatten()
                            .filter(|p| !p.trim().is_empty()),
                        study_pks: Vec::new(),
                    },
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
        if let Some((table, album_col, study_col)) = album_join_table(conn)? {
            let mut stmt = conn.prepare(&format!("SELECT \"{album_col}\", \"{study_col}\" FROM \"{table}\""))?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
                let (a, s) = row?;
                members.entry(a).or_default().push(s);
            }
        }
        for (pk, mut album) in rows {
            album.study_pks = members.remove(&pk).unwrap_or_default();
            albums.push(album);
        }
    }

    Ok(HorosCatalog {
        library: None,
        studies,
        images,
        albums,
    })
}

/// The many-to-many table between albums and studies: a `Z_…` table with
/// one column ending in ALBUMS and one ending in STUDIES.
fn album_join_table(conn: &Connection) -> Result<Option<(String, String, String)>> {
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'Z\\_%' ESCAPE '\\'")?;
    let tables: Vec<String> = stmt.query_map([], |r| r.get(0))?.flatten().collect();
    for t in tables {
        let cols = columns(conn, &t)?;
        let album = cols.iter().find(|c| c.starts_with("Z_") && c.ends_with("ALBUMS"));
        let study = cols.iter().find(|c| c.starts_with("Z_") && c.ends_with("STUDIES"));
        if let (Some(a), Some(s)) = (album, study) {
            return Ok(Some((t, a.clone(), s.clone())));
        }
    }
    Ok(None)
}

/// Turn a Horos smart album rule into a Ruthos filter. Rules that combine
/// plain conditions with AND are converted; anything else (OR, NOT,
/// fields Ruthos does not filter on) returns `None` so the caller can say
/// which albums were left out and why.
pub fn convert_predicate(predicate: &str) -> Option<StudyFilter> {
    let p = predicate.trim();
    if p.is_empty() {
        return None;
    }
    let upper = p.to_ascii_uppercase();
    if upper.contains(" OR ") || upper.contains("NOT ") || upper.contains("ANY ") || upper.contains("SUBQUERY") {
        return None;
    }
    let mut f = StudyFilter::default();
    let mut any = false;
    for clause in split_and(p) {
        let c = clause.trim().trim_start_matches('(').trim_end_matches(')').trim();
        if c.is_empty() {
            continue;
        }
        let (field, rest) = c.split_once(char::is_whitespace)?;
        let field = field.trim().to_ascii_lowercase();
        let rest = rest.trim();
        let (op, value) = split_operator(rest)?;
        let text = quoted(value);
        match field.as_str() {
            "modality" => {
                let m = text?;
                if !matches!(op.as_str(), "==" | "=" | "CONTAINS" | "LIKE" | "BEGINSWITH") || !f.modality.is_empty() {
                    return None;
                }
                f.modality = m.trim_matches('*').to_ascii_uppercase();
            }
            "name" | "patientid" | "studyname" | "accessionnumber" | "id" | "comment" | "institutionname"
            | "referringphysician"
                if text.is_some() && matches!(op.as_str(), "==" | "=" | "CONTAINS" | "LIKE" | "BEGINSWITH") =>
            {
                if !f.text.is_empty() {
                    return None;
                }
                f.text = text?.trim_matches('*').to_string();
            }
            "comment" if matches!(op.as_str(), "!=" | "<>") && (value == "nil" || text.as_deref() == Some("")) => {
                f.has_comment = true;
            }
            "date" if matches!(op.as_str(), ">=" | ">") => {
                f.date = match date_variable(value)? {
                    "TODAY" => DateFilter::Today,
                    "YESTERDAY" | "2DAYS" => DateFilter::LastTwoDays,
                    "WEEK" => DateFilter::LastWeek,
                    "MONTH" => DateFilter::LastMonth,
                    "2MONTHS" => DateFilter::LastTwoMonths,
                    "3MONTHS" => DateFilter::LastThreeMonths,
                    "YEAR" => DateFilter::LastYear,
                    _ => return None,
                };
            }
            "dateadded" | "dateopened" if matches!(op.as_str(), ">=" | ">") => {
                let secs = variable_seconds(date_variable(value)?)?;
                if field == "dateadded" {
                    f.added_within_secs = Some(secs);
                } else {
                    f.opened_within_secs = Some(secs);
                }
            }
            _ => return None,
        }
        any = true;
    }
    any.then_some(f)
}

/// Split on top-level `AND` (also `&&`), outside quotes.
fn split_and(p: &str) -> Vec<String> {
    let normalised = p.replace("&&", " AND ");
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let chars: Vec<char> = normalised.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' {
            in_quote = !in_quote;
        }
        if !in_quote && i + 5 <= chars.len() {
            let w: String = chars[i..i + 5].iter().collect();
            if w.eq_ignore_ascii_case(" AND ") {
                out.push(std::mem::take(&mut cur));
                i += 5;
                continue;
            }
        }
        cur.push(ch);
        i += 1;
    }
    out.push(cur);
    out
}

/// The comparison and the right-hand side: `CONTAINS[cd] "US"` gives
/// (`CONTAINS`, `"US"`).
fn split_operator(rest: &str) -> Option<(String, &str)> {
    for op in [">=", "<=", "!=", "<>", "==", "=", ">", "<"] {
        if let Some(v) = rest.strip_prefix(op) {
            return Some((op.to_string(), v.trim()));
        }
    }
    let (word, v) = rest.split_once(char::is_whitespace)?;
    let word = word.split('[').next().unwrap_or(word).to_ascii_uppercase();
    Some((word, v.trim()))
}

fn quoted(v: &str) -> Option<String> {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    Some(inner.to_string())
}

/// `$NSDATE_WEEK` or `CAST($NSDATE_WEEK, "NSDate")` gives `WEEK`.
fn date_variable(v: &str) -> Option<&str> {
    let i = v.find("$NSDATE_")?;
    let rest = &v[i + "$NSDATE_".len()..];
    let end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn variable_seconds(v: &str) -> Option<i64> {
    const DAY: i64 = 86_400;
    Some(match v {
        "LASTHOUR" => 3_600,
        "LAST6HOURS" => 6 * 3_600,
        "LAST12HOURS" => 12 * 3_600,
        "TODAY" => DAY,
        "YESTERDAY" => 2 * DAY,
        "2DAYS" => 3 * DAY,
        "WEEK" => 8 * DAY,
        "MONTH" => 32 * DAY,
        "2MONTHS" => 63 * DAY,
        "3MONTHS" => 94 * DAY,
        "YEAR" => 366 * DAY,
        _ => return None,
    })
}

/// How the images are brought over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateMode {
    /// Copy the files into Ruthos's database folder; Horos's library is left
    /// as it was.
    Copy,
    /// Use the files where they are (an external drive, say). Ruthos then
    /// depends on that folder staying where it is.
    Link,
}

/// What a migration did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrationSummary {
    pub studies: usize,
    pub new_studies: usize,
    pub images_imported: usize,
    /// Already in Ruthos.
    pub images_present: usize,
    /// Not found on this computer.
    pub images_missing: usize,
    /// Horos's own measurement and report objects, not images.
    pub horos_private_skipped: usize,
    pub errors: Vec<(PathBuf, String)>,
    pub comments: usize,
    pub states: usize,
    pub key_images: usize,
    pub image_comments: usize,
    pub albums_created: usize,
    pub albums_merged: usize,
    pub smart_albums_created: usize,
    /// Smart albums that could not be converted: name and rule.
    pub smart_albums_skipped: Vec<(String, String)>,
    /// Studies with a Horos report file, which is not carried over.
    pub reports_not_copied: usize,
    pub cancelled: bool,
}

/// Bring a Horos library into Ruthos. `progress` is called with images
/// done, the total and a description; setting `cancel` stops between
/// images (what was imported so far stays).
pub fn migrate(
    db: &Database,
    paths: &DataPaths,
    catalog: &HorosCatalog,
    mode: MigrateMode,
    progress: &(dyn Fn(usize, usize, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<MigrationSummary> {
    let import_mode = match mode {
        MigrateMode::Copy => ImportMode::Copy,
        MigrateMode::Link => ImportMode::Link,
    };
    let mut summary = MigrationSummary {
        horos_private_skipped: catalog.images.iter().filter(|i| i.horos_private).count(),
        ..Default::default()
    };
    let work: Vec<&HorosImage> = catalog.images.iter().filter(|i| !i.horos_private).collect();
    let total = work.len();
    let done = AtomicUsize::new(0);

    enum Outcome {
        Imported {
            image_id: i64,
            study_id: i64,
            new_study: bool,
        },
        Present(i64),
        Missing,
        Error(PathBuf, String),
        Skipped,
    }

    let outcomes: Vec<Outcome> = work
        .par_iter()
        .map(|img| {
            if cancel.load(Ordering::Relaxed) {
                return Outcome::Skipped;
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(25) || n == total {
                progress(n, total, &format!("image {n} of {total}"));
            }
            let Some(file) = &img.file else {
                // Not on this computer; perhaps already in Ruthos.
                return match img
                    .sop_instance_uid
                    .as_deref()
                    .and_then(|u| db.image_by_sop_uid(u).ok().flatten())
                {
                    Some(row) => Outcome::Present(row.id),
                    None => Outcome::Missing,
                };
            };
            match import_file(db, paths, file, import_mode) {
                Ok(FileOutcome::Imported {
                    image_id,
                    study_id,
                    new_study,
                    ..
                }) => Outcome::Imported {
                    image_id,
                    study_id,
                    new_study,
                },
                Ok(FileOutcome::Duplicate) => {
                    let uid = img
                        .sop_instance_uid
                        .clone()
                        .or_else(|| ruthos_dicom::read_meta(file).ok().map(|m| m.sop_instance_uid));
                    match uid.and_then(|u| db.image_by_sop_uid(&u).ok().flatten()) {
                        Some(row) => Outcome::Present(row.id),
                        None => Outcome::Error(file.clone(), "already in the archive under another path".into()),
                    }
                }
                Ok(FileOutcome::NotDicom) => Outcome::Error(file.clone(), "not a DICOM file".into()),
                Err(e) => Outcome::Error(file.clone(), e.to_string()),
            }
        })
        .collect();
    summary.cancelled = cancel.load(Ordering::Relaxed);

    // Images to Ruthos ids; key images and image comments.
    let mut new_studies: HashSet<i64> = HashSet::new();
    for (img, outcome) in work.iter().zip(outcomes) {
        let image_id = match outcome {
            Outcome::Imported {
                image_id,
                study_id,
                new_study,
            } => {
                summary.images_imported += 1;
                if new_study {
                    new_studies.insert(study_id);
                }
                image_id
            }
            Outcome::Present(id) => {
                summary.images_present += 1;
                id
            }
            Outcome::Missing => {
                summary.images_missing += 1;
                continue;
            }
            Outcome::Error(f, e) => {
                summary.errors.push((f, e));
                continue;
            }
            Outcome::Skipped => continue,
        };
        if img.key_image {
            db.set_key_image(image_id, true)?;
            summary.key_images += 1;
        }
        if !img.comment.is_empty() && db.set_image_comment_if_empty(image_id, &img.comment)? {
            summary.image_comments += 1;
        }
    }
    summary.new_studies = new_studies.len();

    // Studies: comment, status and the dates Horos recorded.
    let mut study_ids: HashMap<i64, i64> = HashMap::new();
    for s in &catalog.studies {
        if s.study_instance_uid.is_empty() {
            continue;
        }
        let Some(id) = db.study_id_for_uid(&s.study_instance_uid)? else {
            continue;
        };
        study_ids.insert(s.pk, id);
        summary.studies += 1;
        if s.has_report {
            summary.reports_not_copied += 1;
        }
        let Some(row) = db.study(id)? else { continue };
        if !s.comment.is_empty() && row.comment.trim().is_empty() {
            db.set_study_comment(id, &s.comment)?;
            summary.comments += 1;
        }
        if s.state > 0 && row.state == 0 {
            db.set_study_state(id, s.state)?;
            summary.states += 1;
        }
        // A study this migration created keeps Horos's dates, so "Just
        // Added" does not fill with the whole Horos library.
        if new_studies.contains(&id) {
            db.set_study_dates(id, s.date_added, s.date_opened)?;
        }
    }

    // Albums.
    let existing = db.albums()?;
    for a in &catalog.albums {
        let name = a.name.trim();
        if name.is_empty() {
            continue;
        }
        if a.smart {
            if BUILT_IN_ALBUMS.iter().any(|b| name.starts_with(b)) {
                continue;
            }
            let rule = a.predicate.clone().unwrap_or_default();
            match convert_predicate(&rule) {
                Some(filter) => {
                    if !existing.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
                        db.create_album(name, Some(&filter))?;
                        summary.smart_albums_created += 1;
                    }
                }
                None => summary.smart_albums_skipped.push((name.to_string(), rule)),
            }
            continue;
        }
        let members: Vec<i64> = a.study_pks.iter().filter_map(|pk| study_ids.get(pk).copied()).collect();
        let album_id = match existing
            .iter()
            .find(|e| !e.is_smart && e.name.eq_ignore_ascii_case(name))
        {
            Some(e) => {
                summary.albums_merged += 1;
                e.id
            }
            None => {
                summary.albums_created += 1;
                db.create_album(name, None)?
            }
        };
        for s in members {
            db.add_study_to_album(album_id, s)?;
        }
    }
    progress(total, total, "finished");
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sop_uids_decode() {
        // "1.2.3" -> nibbles 2,11,3,11,4,0
        assert_eq!(decode_sop_uid(&[0x2b, 0x3b, 0x40]).as_deref(), Some("1.2.3"));
        assert_eq!(decode_sop_uid(&[0x21, 0xb0]).as_deref(), Some("10."));
        assert_eq!(decode_sop_uid(&[0xff]), None);
    }

    #[test]
    fn smart_album_rules_convert() {
        let f = convert_predicate("(modality CONTAINS[cd] \"US\")").unwrap();
        assert_eq!(f.modality, "US");
        let f = convert_predicate("(modality == \"CR\") AND (date >= CAST($NSDATE_WEEK, \"NSDate\"))").unwrap();
        assert_eq!((f.modality.as_str(), f.date), ("CR", DateFilter::LastWeek));
        let f = convert_predicate("name CONTAINS[cd] \"bertie\"").unwrap();
        assert_eq!(f.text, "bertie");
        let f = convert_predicate("(comment != nil)").unwrap();
        assert!(f.has_comment);
        let f = convert_predicate("dateAdded >= CAST($NSDATE_LASTHOUR, \"NSDate\")").unwrap();
        assert_eq!(f.added_within_secs, Some(3600));
        assert!(convert_predicate("(modality == \"CT\") OR (modality == \"MR\")").is_none());
        assert!(convert_predicate("(numberOfImages > 100)").is_none());
        assert!(convert_predicate("").is_none());
    }

    #[test]
    fn mac_paths_inside_a_library_resolve_to_the_copy() {
        let dir = std::env::temp_dir().join(format!("ruthos-horos-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Imported")).unwrap();
        std::fs::write(dir.join("Imported").join("a.dcm"), b"x").unwrap();
        let (f, _) = resolve_file(
            &dir,
            &HashMap::new(),
            None,
            Some("/Users/vet/Documents/Horos Data/Imported/a.dcm"),
        );
        assert_eq!(f, Some(dir.join("Imported").join("a.dcm")));
        let (f, recorded) = resolve_file(&dir, &HashMap::new(), None, Some("/Volumes/USB/x.dcm"));
        assert_eq!((f, recorded.as_str()), (None, "/Volumes/USB/x.dcm"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
