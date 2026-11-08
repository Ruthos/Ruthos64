//! SQLite storage for the archive.
//!
//! Schema follows the Horos model: Study, Series, Image, Album. Counts are
//! derived with subqueries rather than cached, which keeps inserts simple and
//! is fast enough for tens of thousands of studies.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use ruthos_dicom::meta::{dicom_date_to_iso, dicom_time_to_hms, InstanceMeta};

use crate::mmode::MmodeRow;
use crate::model::{
    timeline_order, AlbumRow, DateFilter, ImageRow, MeasurementInput, MeasurementRow, ReportRow, RoiRecord, RoiValue,
    SeriesRow, StudyRow, TimelineEntry,
};
use crate::paths::DataPaths;
use crate::teaching::TeachingCase;
use crate::Result;

const SCHEMA_VERSION: i64 = 3;

/// A hand calibration (`[row mm, column mm]`) and the image point it was
/// set from when it is scoped to an ultrasound region.
pub type ScopedCalibration = ([f64; 2], Option<(f64, f64)>);

pub struct Database {
    conn: Mutex<Connection>,
    pub paths: DataPaths,
}

/// Outcome of inserting one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted {
        study_id: i64,
        series_id: i64,
        image_id: i64,
        new_study: bool,
    },
    Duplicate,
}

/// Sort order for the study list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StudySort {
    #[default]
    DateAddedDesc,
    StudyDateDesc,
    PatientName,
    PatientId,
    Modality,
}

/// A column of the study list that can be sorted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudyColumn {
    PatientName,
    Report,
    PatientId,
    Age,
    Accession,
    StudyDate,
    Modality,
    Description,
    Images,
    DateAdded,
    Comments,
    Status,
}

/// A date as `YYYY-MM-DD`, whether stored that way or as a DICOM `YYYYMMDD`.
macro_rules! iso {
    ($column:literal) => {
        concat!(
            "date(substr(replace(",
            $column,
            ",'-',''),1,4)||'-'||substr(replace(",
            $column,
            ",'-',''),5,2)||'-'||substr(replace(",
            $column,
            ",'-',''),7,2))"
        )
    };
}

/// Days old at the study, or nothing if the dates do not make sense.
const AGE_IN_DAYS: &str = concat!(
    "CASE WHEN julianday(",
    iso!("s.study_date"),
    ") >= julianday(",
    iso!("s.patient_birth_date"),
    ") THEN julianday(",
    iso!("s.study_date"),
    ") - julianday(",
    iso!("s.patient_birth_date"),
    ") END"
);

impl StudyColumn {
    pub fn title(self) -> &'static str {
        match self {
            Self::PatientName => "Patient Name",
            Self::Report => "Report",
            Self::PatientId => "Patient ID",
            Self::Age => "Age",
            Self::Accession => "Accession",
            Self::StudyDate => "Study Date",
            Self::Modality => "Modality",
            Self::Description => "Description",
            Self::Images => "Images",
            Self::DateAdded => "Date Added",
            Self::Comments => "Comments",
            Self::Status => "Status",
        }
    }

    /// Which way a first click sorts: newest and largest first where that is
    /// what people look for, A to Z otherwise.
    fn starts_descending(self) -> bool {
        matches!(
            self,
            Self::Report | Self::StudyDate | Self::Images | Self::DateAdded | Self::Status
        )
    }

    /// The value to sort on, and what makes a row blank for it.
    fn key(self) -> (&'static str, String) {
        let text = |column: &'static str, key: &'static str| (key, format!("IFNULL({column}, '') = ''"));
        match self {
            Self::PatientName => text("s.patient_name", "s.patient_name COLLATE NOCASE"),
            Self::PatientId => text("s.patient_id", "s.patient_id COLLATE NOCASE"),
            Self::Accession => text("s.accession_number", "s.accession_number COLLATE NOCASE"),
            Self::StudyDate => text("s.study_date", "s.study_date"),
            Self::Modality => text("s.modalities", "s.modalities COLLATE NOCASE"),
            Self::Description => text("s.study_description", "s.study_description COLLATE NOCASE"),
            Self::Comments => text("s.comment", "s.comment COLLATE NOCASE"),
            Self::Age => (AGE_IN_DAYS, format!("({AGE_IN_DAYS}) IS NULL")),
            Self::Report => (
                "EXISTS (SELECT 1 FROM report r WHERE r.study_id = s.id AND (r.findings <> '' OR r.conclusion <> ''))",
                "1 = 0".to_string(),
            ),
            Self::Images => (
                "(SELECT COUNT(*) FROM image i JOIN series se ON i.series_id = se.id WHERE se.study_id = s.id)",
                "1 = 0".to_string(),
            ),
            Self::DateAdded => ("s.date_added", "1 = 0".to_string()),
            Self::Status => ("s.state", "1 = 0".to_string()),
        }
    }
}

/// How the study list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyOrder {
    pub column: StudyColumn,
    pub descending: bool,
}

impl StudyOrder {
    /// The order after a click on a heading: its usual way first, then back.
    pub fn after_click(self, column: StudyColumn) -> Self {
        Self {
            column,
            descending: if column == self.column {
                !self.descending
            } else {
                column.starts_descending()
            },
        }
    }

    /// Blank values stay last whichever way the list runs.
    fn order_by(self) -> String {
        let way = if self.descending { "DESC" } else { "ASC" };
        let (key, blank) = self.column.key();
        let settle = match self.column {
            StudyColumn::StudyDate => format!("s.study_time {way}, s.id {way}"),
            StudyColumn::DateAdded => format!("s.id {way}"),
            _ => "s.study_date DESC, s.study_time DESC, s.id DESC".to_string(),
        };
        format!(" ORDER BY {blank}, {key} {way}, {settle}")
    }
}

impl From<StudySort> for StudyOrder {
    fn from(sort: StudySort) -> Self {
        let (column, descending) = match sort {
            StudySort::DateAddedDesc => (StudyColumn::DateAdded, true),
            StudySort::StudyDateDesc => (StudyColumn::StudyDate, true),
            StudySort::PatientName => (StudyColumn::PatientName, false),
            StudySort::PatientId => (StudyColumn::PatientId, false),
            StudySort::Modality => (StudyColumn::Modality, false),
        };
        Self { column, descending }
    }
}

/// What the database window's search field, modality popup, date popup and
/// album selection add up to. Smart albums store one of these as JSON.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StudyFilter {
    /// Free text matched against patient name, patient ID, description,
    /// accession number, comment, species and breed.
    pub text: String,
    /// Exact modality, e.g. `CR`. Empty means any.
    pub modality: String,
    pub date: DateFilter,
    /// Restrict to an album's studies.
    #[serde(skip)]
    pub album_id: Option<i64>,
    pub sort: StudySort,
    /// A heading clicked in the study list, in place of `sort`.
    pub order: Option<StudyOrder>,
    /// Only studies with a comment (Horos: "Cases with comments").
    pub has_comment: bool,
    /// Only studies added in the last so many seconds ("Just Added").
    pub added_within_secs: Option<i64>,
    /// Only studies opened in the last so many seconds ("Just Opened").
    pub opened_within_secs: Option<i64>,
    /// Only studies with an image on this catalogued drive.
    #[serde(skip)]
    pub catalogue_root: Option<String>,
}

impl StudyFilter {
    /// The order the list is in, from a heading or else the Sort menu.
    pub fn shown_order(&self) -> StudyOrder {
        self.order.unwrap_or_else(|| self.sort.into())
    }
}

impl Database {
    pub fn open(paths: &DataPaths) -> Result<Arc<Self>> {
        paths.ensure()?;
        let conn = Connection::open(paths.database_file())?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            paths: paths.clone(),
        }))
    }

    /// An in-memory database for tests.
    pub fn open_in_memory(paths: &DataPaths) -> Result<Arc<Self>> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            paths: paths.clone(),
        }))
    }

    // ----------------------------------------------------------------- insert

    pub fn insert_instance(
        &self,
        meta: &InstanceMeta,
        stored_path: &str,
        stored_in_database_folder: bool,
        file_size: i64,
    ) -> Result<InsertOutcome> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let now = now_unix();

        // Fast path: already have this instance.
        let exists: Option<i64> = tx
            .query_row(
                "SELECT id FROM image WHERE sop_instance_uid = ?1",
                params![meta.sop_instance_uid],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Ok(InsertOutcome::Duplicate);
        }

        // Study
        let existing_study: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, modalities FROM study WHERE study_instance_uid = ?1",
                params![meta.study_instance_uid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (study_id, new_study) = match existing_study {
            Some((id, modalities)) => {
                let merged = merge_modality(&modalities, &meta.modality);
                if merged != modalities {
                    tx.execute("UPDATE study SET modalities = ?1 WHERE id = ?2", params![merged, id])?;
                }
                (id, false)
            }
            None => {
                tx.execute(
                    "INSERT INTO study (study_instance_uid, patient_name, patient_name_raw, patient_id,
                        patient_birth_date, patient_sex, patient_species, patient_breed, responsible_person,
                        study_date, study_time, study_description, accession_number, referring_physician,
                        institution_name, study_id_tag, modalities, date_added, comment, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, '', 0)",
                    params![
                        meta.study_instance_uid,
                        meta.patient_name.display(),
                        meta.patient_name.raw,
                        meta.patient_id,
                        meta.patient_birth_date.as_deref().and_then(dicom_date_to_iso),
                        meta.patient_sex,
                        meta.patient_species,
                        meta.patient_breed,
                        meta.responsible_person,
                        meta.study_date.as_deref().and_then(dicom_date_to_iso),
                        meta.study_time.as_deref().and_then(dicom_time_to_hms),
                        meta.study_description,
                        meta.accession_number,
                        meta.referring_physician,
                        meta.institution_name,
                        meta.study_id,
                        meta.modality,
                        now,
                    ],
                )?;
                (tx.last_insert_rowid(), true)
            }
        };

        // Series. A multi-frame file (an echo loop, a fluoroscopy run) gets a
        // series of its own, as Horos does, so each loop opens as a stack of
        // frames instead of the loops stacking as images.
        // A SEG's frames are not a loop, and its versions share one series.
        let multiframe = meta.number_of_frames > 1 && meta.modality != crate::seg::MODALITY;
        let series_uid = if multiframe {
            format!("{}#{}", meta.series_instance_uid, meta.sop_instance_uid)
        } else {
            meta.series_instance_uid.clone()
        };
        let series_description = if multiframe {
            let base = if meta.series_description.is_empty() {
                match meta.series_number {
                    Some(n) => format!("Series {n}"),
                    None => "Series".to_string(),
                }
            } else {
                meta.series_description.clone()
            };
            match meta.instance_number {
                Some(n) => format!("{base} - loop {n}"),
                None => format!("{base} - loop ({} frames)", meta.number_of_frames),
            }
        } else {
            meta.series_description.clone()
        };
        let existing_series: Option<i64> = tx
            .query_row(
                "SELECT id FROM series WHERE series_instance_uid = ?1",
                params![series_uid],
                |r| r.get(0),
            )
            .optional()?;
        let series_id = match existing_series {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO series (study_id, series_instance_uid, series_number, modality,
                        series_description, series_date, body_part_examined, view_position, laterality,
                        window_center, window_width, rotation_angle, x_flipped, y_flipped, comment, date_added)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0, 0, '', ?12)",
                    params![
                        study_id,
                        series_uid,
                        meta.series_number,
                        meta.modality,
                        series_description,
                        meta.series_date.as_deref().and_then(dicom_date_to_iso),
                        meta.body_part_examined,
                        meta.view_position,
                        meta.laterality,
                        meta.window_center,
                        meta.window_width,
                        now,
                    ],
                )?;
                tx.last_insert_rowid()
            }
        };

        // Image
        tx.execute(
            "INSERT INTO image (series_id, sop_instance_uid, sop_class_uid, instance_number, number_of_frames,
                rows, columns, path, stored_in_database_folder, file_size, transfer_syntax, is_key_image,
                slice_location, comment, date_added, acquired)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, '', ?13, ?14)",
            params![
                series_id,
                meta.sop_instance_uid,
                meta.sop_class_uid,
                meta.instance_number,
                meta.number_of_frames as i64,
                meta.rows as i64,
                meta.columns as i64,
                stored_path,
                stored_in_database_folder,
                file_size,
                meta.transfer_syntax,
                meta.slice_location,
                now,
                meta.acquired().unwrap_or_default(),
            ],
        )?;
        let image_id = tx.last_insert_rowid();
        tx.commit()?;

        Ok(InsertOutcome::Inserted {
            study_id,
            series_id,
            image_id,
            new_study,
        })
    }

    // ---------------------------------------------------------------- studies

    pub fn list_studies(&self, filter: &StudyFilter) -> Result<Vec<StudyRow>> {
        let conn = self.conn.lock();
        let mut sql = String::from(
            "SELECT s.id, s.study_instance_uid, s.patient_name, s.patient_id, s.patient_birth_date,
                    s.patient_sex, s.patient_species, s.patient_breed, s.responsible_person,
                    s.study_date, s.study_time, s.study_description, s.accession_number,
                    s.referring_physician, s.institution_name, s.modalities, s.date_added,
                    s.date_opened, s.comment, s.state,
                    (SELECT COUNT(*) FROM series se WHERE se.study_id = s.id),
                    (SELECT COUNT(*) FROM image i JOIN series se ON i.series_id = se.id WHERE se.study_id = s.id),
                    s.teaching_reference, s.teaching_answer_key
             FROM study s",
        );
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(album_id) = filter.album_id {
            sql.push_str(" JOIN album_study als ON als.study_id = s.id");
            clauses.push(format!("als.album_id = ?{}", args.len() + 1));
            args.push(album_id.into());
        }
        let text = filter.text.trim();
        if !text.is_empty() {
            let like = format!("%{}%", text.replace('%', "\\%"));
            let n = args.len() + 1;
            clauses.push(format!(
                "(s.patient_name LIKE ?{n} ESCAPE '\\' OR s.patient_id LIKE ?{n} ESCAPE '\\'
                  OR s.study_description LIKE ?{n} ESCAPE '\\' OR s.accession_number LIKE ?{n} ESCAPE '\\'
                  OR s.comment LIKE ?{n} ESCAPE '\\' OR IFNULL(s.patient_species,'') LIKE ?{n} ESCAPE '\\'
                  OR IFNULL(s.patient_breed,'') LIKE ?{n} ESCAPE '\\'
                  OR IFNULL(s.responsible_person,'') LIKE ?{n} ESCAPE '\\')"
            ));
            args.push(like.into());
        }
        if !filter.modality.trim().is_empty() {
            let n = args.len() + 1;
            clauses.push(format!("('\\' || s.modalities || '\\') LIKE ?{n}"));
            args.push(format!("%\\{}\\%", filter.modality.trim()).into());
        }
        if let Some((from, to)) = filter.date.bounds(chrono::Local::now().date_naive()) {
            let n = args.len() + 1;
            clauses.push(format!("s.study_date BETWEEN ?{n} AND ?{}", n + 1));
            args.push(from.into());
            args.push(to.into());
        }
        if filter.has_comment {
            clauses.push("s.comment <> ''".to_string());
        }
        if let Some(secs) = filter.added_within_secs {
            let n = args.len() + 1;
            clauses.push(format!("s.date_added >= ?{n}"));
            args.push((now_unix() - secs).into());
        }
        if let Some(secs) = filter.opened_within_secs {
            let n = args.len() + 1;
            clauses.push(format!("s.date_opened >= ?{n}"));
            args.push((now_unix() - secs).into());
        }
        if let Some(root) = &filter.catalogue_root {
            let n = args.len() + 1;
            clauses.push(format!(
                "s.id IN (SELECT se.study_id FROM series se JOIN image i ON i.series_id = se.id
                          WHERE i.stored_in_database_folder = 0 AND substr(i.path, 1, length(?{n})) = ?{n})"
            ));
            args.push(root.clone().into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(&filter.shown_order().order_by());

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), study_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The study holding this Study Instance UID, if any.
    pub fn study_id_by_uid(&self, uid: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT id FROM study WHERE study_instance_uid = ?1",
                params![uid],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn study(&self, id: i64) -> Result<Option<StudyRow>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT s.id, s.study_instance_uid, s.patient_name, s.patient_id, s.patient_birth_date,
                        s.patient_sex, s.patient_species, s.patient_breed, s.responsible_person,
                        s.study_date, s.study_time, s.study_description, s.accession_number,
                        s.referring_physician, s.institution_name, s.modalities, s.date_added,
                        s.date_opened, s.comment, s.state,
                        (SELECT COUNT(*) FROM series se WHERE se.study_id = s.id),
                        (SELECT COUNT(*) FROM image i JOIN series se ON i.series_id = se.id WHERE se.study_id = s.id),
                    s.teaching_reference, s.teaching_answer_key
                 FROM study s WHERE s.id = ?1",
                params![id],
                study_from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn set_study_comment(&self, id: i64, comment: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("UPDATE study SET comment = ?1 WHERE id = ?2", params![comment, id])?;
        Ok(())
    }

    pub fn set_study_state(&self, id: i64, state: i64) -> Result<()> {
        self.conn
            .lock()
            .execute("UPDATE study SET state = ?1 WHERE id = ?2", params![state, id])?;
        Ok(())
    }

    /// Set the dates a study was added and last opened (Unix seconds), as
    /// recorded by another archive. `None` leaves a date as it is.
    pub fn set_study_dates(&self, id: i64, added: Option<i64>, opened: Option<i64>) -> Result<()> {
        let conn = self.conn.lock();
        if let Some(a) = added {
            conn.execute("UPDATE study SET date_added = ?1 WHERE id = ?2", params![a, id])?;
        }
        if let Some(o) = opened {
            conn.execute("UPDATE study SET date_opened = ?1 WHERE id = ?2", params![o, id])?;
        }
        Ok(())
    }

    /// Set an image's comment unless it already has one. Returns whether
    /// it was set.
    pub fn set_image_comment_if_empty(&self, image_id: i64, comment: &str) -> Result<bool> {
        let n = self.conn.lock().execute(
            "UPDATE image SET comment = ?1 WHERE id = ?2 AND TRIM(comment) = ''",
            params![comment, image_id],
        )?;
        Ok(n > 0)
    }

    /// Whether a watched folder's file was already looked at in this form.
    pub fn watched_file_seen(&self, path: &str, size: i64, mtime: i64) -> Result<bool> {
        let n: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM watched_file WHERE path = ?1 AND size = ?2 AND mtime = ?3",
            params![path, size, mtime],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn mark_watched_file(&self, path: &str, size: i64, mtime: i64) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO watched_file (path, size, mtime) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime",
            params![path, size, mtime],
        )?;
        Ok(())
    }

    pub fn mark_study_opened(&self, id: i64) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE study SET date_opened = ?1 WHERE id = ?2",
            params![now_unix(), id],
        )?;
        Ok(())
    }

    /// Record a catalogued drive, or note that it has been scanned again.
    pub fn add_catalogue(&self, name: &str, root: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO catalogue (name, root, scanned_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(root) DO UPDATE SET scanned_at = excluded.scanned_at",
            params![name, root, now_unix()],
        )?;
        Ok(())
    }

    pub fn catalogues(&self) -> Result<Vec<crate::catalogue::Catalogue>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.root, c.scanned_at,
                    (SELECT COUNT(*) FROM image i WHERE i.stored_in_database_folder = 0
                       AND substr(i.path, 1, length(c.root)) = c.root)
             FROM catalogue c ORDER BY c.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(crate::catalogue::Catalogue {
                id: r.get(0)?,
                name: r.get(1)?,
                root: r.get(2)?,
                scanned_at: r.get(3)?,
                images: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// A few images of a catalogue, with their sizes, to recognise it by.
    pub fn catalogue_samples(&self, root: &str, count: usize) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT path, file_size FROM image
             WHERE stored_in_database_folder = 0 AND substr(path, 1, length(?1)) = ?1
             ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![root, count as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Point a catalogue's images at where the drive is now. False if another
    /// catalogue already has that place.
    pub fn move_catalogue(&self, id: i64, old_root: &str, new_root: &str) -> Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let taken: i64 = tx.query_row(
            "SELECT COUNT(*) FROM catalogue WHERE root = ?1",
            params![new_root],
            |r| r.get(0),
        )?;
        if taken > 0 {
            return Ok(false);
        }
        tx.execute(
            "UPDATE image SET path = ?2 || substr(path, length(?1) + 1)
             WHERE stored_in_database_folder = 0 AND substr(path, 1, length(?1)) = ?1",
            params![old_root, new_root],
        )?;
        tx.execute("UPDATE catalogue SET root = ?2 WHERE id = ?1", params![id, new_root])?;
        tx.commit()?;
        Ok(true)
    }

    /// Stop cataloguing a drive: its studies leave the database and no file is
    /// touched. A study also holding images from elsewhere stays. Returns the
    /// studies removed.
    pub fn forget_catalogue(&self, catalogue: &crate::catalogue::Catalogue) -> Result<Vec<i64>> {
        let studies: Vec<i64> = {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare(
                "SELECT se.study_id FROM series se JOIN image i ON i.series_id = se.id
                 GROUP BY se.study_id
                 HAVING SUM(i.stored_in_database_folder = 0 AND substr(i.path, 1, length(?1)) = ?1) = COUNT(*)",
            )?;
            let rows = stmt.query_map(params![catalogue.root], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in &studies {
            self.delete_study(*id)?;
        }
        self.conn
            .lock()
            .execute("DELETE FROM catalogue WHERE id = ?1", params![catalogue.id])?;
        Ok(studies)
    }

    /// A study's first image: where it is and whether it is in the database folder.
    pub fn first_image(&self, study_id: i64) -> Result<Option<(String, bool)>> {
        let conn = self.conn.lock();
        let found = conn
            .query_row(
                "SELECT i.path, i.stored_in_database_folder FROM image i
                 JOIN series se ON i.series_id = se.id WHERE se.study_id = ?1 ORDER BY i.id LIMIT 1",
                params![study_id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1)),
            )
            .optional()?;
        Ok(found)
    }

    /// The name of the catalogued drive a file is on.
    pub fn catalogue_of(&self, path: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let found = conn
            .query_row(
                "SELECT name FROM catalogue WHERE substr(?1, 1, length(root)) = root",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// Delete a study and return the files that were stored inside the
    /// database folder so the caller can remove them from disk.
    pub fn delete_study(&self, id: i64) -> Result<Vec<PathBuf>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let files: Vec<PathBuf> = {
            let mut stmt = tx.prepare(
                "SELECT i.path FROM image i JOIN series se ON i.series_id = se.id
                 WHERE se.study_id = ?1 AND i.stored_in_database_folder = 1",
            )?;
            let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).map(|p| self.paths.resolve(&p)).collect()
        };
        tx.execute("DELETE FROM album_study WHERE study_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM image WHERE series_id IN (SELECT id FROM series WHERE study_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM series WHERE study_id = ?1", params![id])?;
        tx.execute("DELETE FROM study WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(files)
    }

    pub fn modalities(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT modality FROM series WHERE modality <> '' ORDER BY modality")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// (studies, series, images)
    pub fn counts(&self) -> Result<(i64, i64, i64)> {
        let conn = self.conn.lock();
        let s: i64 = conn.query_row("SELECT COUNT(*) FROM study", [], |r| r.get(0))?;
        let se: i64 = conn.query_row("SELECT COUNT(*) FROM series", [], |r| r.get(0))?;
        let i: i64 = conn.query_row("SELECT COUNT(*) FROM image", [], |r| r.get(0))?;
        Ok((s, se, i))
    }

    // ----------------------------------------------------------------- series

    pub fn series_for_study(&self, study_id: i64) -> Result<Vec<SeriesRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT se.id, se.study_id, se.series_instance_uid, se.series_number, se.modality,
                    se.series_description, se.series_date, se.body_part_examined, se.view_position,
                    se.laterality,
                    (SELECT COUNT(*) FROM image i WHERE i.series_id = se.id),
                    (SELECT COUNT(*) FROM image i WHERE i.series_id = se.id AND i.is_key_image = 1),
                    se.window_center, se.window_width, se.rotation_angle, se.x_flipped, se.y_flipped,
                    se.comment, se.date_added,
                    (SELECT IFNULL(SUM(i.number_of_frames), 0) FROM image i WHERE i.series_id = se.id)
             FROM series se WHERE se.study_id = ?1
             ORDER BY se.series_number, se.id",
        )?;
        let rows = stmt.query_map(params![study_id], series_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn series(&self, id: i64) -> Result<Option<SeriesRow>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT se.id, se.study_id, se.series_instance_uid, se.series_number, se.modality,
                        se.series_description, se.series_date, se.body_part_examined, se.view_position,
                        se.laterality,
                        (SELECT COUNT(*) FROM image i WHERE i.series_id = se.id),
                        (SELECT COUNT(*) FROM image i WHERE i.series_id = se.id AND i.is_key_image = 1),
                        se.window_center, se.window_width, se.rotation_angle, se.x_flipped, se.y_flipped,
                        se.comment, se.date_added,
                        (SELECT IFNULL(SUM(i.number_of_frames), 0) FROM image i WHERE i.series_id = se.id)
                 FROM series se WHERE se.id = ?1",
                params![id],
                series_from_row,
            )
            .optional()?)
    }

    pub fn set_series_display(
        &self,
        id: i64,
        window_center: Option<f64>,
        window_width: Option<f64>,
        rotation_angle: f64,
        x_flipped: bool,
        y_flipped: bool,
    ) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE series SET window_center = ?1, window_width = ?2, rotation_angle = ?3,
                    x_flipped = ?4, y_flipped = ?5 WHERE id = ?6",
            params![window_center, window_width, rotation_angle, x_flipped, y_flipped, id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------- meta

    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .lock()
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn image_by_sop_uid(&self, sop_instance_uid: &str) -> Result<Option<ImageRow>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT id, series_id, sop_instance_uid, sop_class_uid, instance_number, number_of_frames,
                        rows, columns, path, stored_in_database_folder, file_size, transfer_syntax,
                        is_key_image, slice_location, comment, date_added
                 FROM image WHERE sop_instance_uid = ?1",
                params![sop_instance_uid],
                image_from_row,
            )
            .optional()?)
    }

    // --------------------------------------------------------------- teaching

    const TEACHING_COLS: &'static str =
        "id, reference, name, title, secret, offset_days, preserve_season, note, created";

    pub fn teaching_cases(&self) -> Result<Vec<TeachingCase>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM teaching_case ORDER BY created DESC, id DESC",
            Self::TEACHING_COLS
        ))?;
        let rows = stmt.query_map([], teaching_case_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn teaching_case(&self, id: i64) -> Result<Option<TeachingCase>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM teaching_case WHERE id = ?1", Self::TEACHING_COLS),
                params![id],
                teaching_case_from_row,
            )
            .optional()?)
    }

    pub fn teaching_case_by_reference(&self, reference: &str) -> Result<Option<TeachingCase>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM teaching_case WHERE reference = ?1", Self::TEACHING_COLS),
                params![reference],
                teaching_case_from_row,
            )
            .optional()?)
    }

    pub fn create_teaching_case(
        &self,
        reference: &str,
        name: &str,
        title: &str,
        secret: &[u8; 32],
        offset_days: i64,
        preserve_season: bool,
    ) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO teaching_case (reference, name, title, secret, offset_days, preserve_season, note, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7)",
            params![
                reference,
                name,
                title,
                secret.to_vec(),
                offset_days,
                preserve_season as i64,
                now_unix()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_teaching_case_note(&self, id: i64, note: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("UPDATE teaching_case SET note = ?1 WHERE id = ?2", params![note, id])?;
        Ok(())
    }

    pub fn add_teaching_source(&self, case_id: i64, study_instance_uid: &str, study_id: Option<i64>) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO teaching_source (case_id, study_instance_uid, study_id) VALUES (?1, ?2, ?3)",
            params![case_id, study_instance_uid, study_id],
        )?;
        Ok(())
    }

    /// Source study UIDs of a case.
    pub fn teaching_sources(&self, case_id: i64) -> Result<Vec<(String, Option<i64>)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT study_instance_uid, study_id FROM teaching_source WHERE case_id = ?1")?;
        let rows = stmt.query_map(params![case_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Cases a source study has been exported under (visible only locally).
    pub fn teaching_cases_for_study(&self, study_instance_uid: &str) -> Result<Vec<TeachingCase>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM teaching_case c JOIN teaching_source s ON s.case_id = c.id WHERE s.study_instance_uid = ?1 ORDER BY c.id",
            Self::TEACHING_COLS.split(", ").map(|c| format!("c.{c}")).collect::<Vec<_>>().join(", ")
        ))?;
        let rows = stmt.query_map(params![study_instance_uid], teaching_case_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Study Instance UIDs that have at least one teaching case.
    pub fn teaching_source_uids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT study_instance_uid FROM teaching_source")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn record_teaching_export(
        &self,
        case_id: i64,
        study_instance_uid: &str,
        destination: &str,
        result: &str,
        series: i64,
        images: i64,
    ) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO teaching_export (case_id, study_instance_uid, time, destination, result, series, images)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                case_id,
                study_instance_uid,
                now_unix(),
                destination,
                result,
                series,
                images
            ],
        )?;
        Ok(())
    }

    pub fn teaching_export_count(&self, case_id: i64) -> Result<i64> {
        Ok(self.conn.lock().query_row(
            "SELECT COUNT(*) FROM teaching_export WHERE case_id = ?1",
            params![case_id],
            |r| r.get(0),
        )?)
    }

    pub fn delete_teaching_case(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM teaching_export WHERE case_id = ?1", params![id])?;
        conn.execute("DELETE FROM teaching_source WHERE case_id = ?1", params![id])?;
        conn.execute("DELETE FROM teaching_case WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_study_teaching(&self, study_id: i64, reference: &str, answer_key: bool) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE study SET teaching_reference = ?1, teaching_answer_key = ?2 WHERE id = ?3",
            params![reference, answer_key as i64, study_id],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------- rois

    /// ROIs stored for one frame, oldest first.
    pub fn rois(&self, image_id: i64, frame: i64) -> Result<Vec<RoiRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, image_id, frame, kind, points, name, value, unit, basis, def_key FROM roi
             WHERE image_id = ?1 AND frame = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![image_id, frame], roi_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replace every ROI of one frame with `rois`. Rows whose record carries
    /// an id are updated in place, keeping the id, name and creation time;
    /// new ones are inserted and the record's id is filled in; rows no longer
    /// present are deleted.
    pub fn replace_rois(&self, image_id: i64, frame: i64, rois: &[RoiRecord]) -> Result<()> {
        let mut copy: Vec<RoiRecord> = rois.to_vec();
        self.sync_rois(image_id, frame, &mut copy)
    }

    pub fn sync_rois(&self, image_id: i64, frame: i64, rois: &mut [RoiRecord]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let existing: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM roi WHERE image_id = ?1 AND frame = ?2")?;
            let ids = stmt.query_map(params![image_id, frame], |r| r.get::<_, i64>(0))?;
            ids.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let keep: Vec<i64> = rois.iter().map(|r| r.id).filter(|id| *id > 0).collect();
        for id in existing.iter().filter(|id| !keep.contains(id)) {
            tx.execute("DELETE FROM roi WHERE id = ?1", params![id])?;
        }
        for r in rois.iter_mut() {
            let points = serde_json::to_string(&r.points).unwrap_or_else(|_| "[]".into());
            if r.id > 0 && existing.contains(&r.id) {
                tx.execute(
                    "UPDATE roi SET kind = ?1, points = ?2, name = ?3, value = ?4, unit = ?5, basis = ?6, date_modified = ?7, def_key = ?9 WHERE id = ?8",
                    params![r.kind, points, r.name, r.value, r.unit, r.basis, now_unix(), r.id, r.def_key],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO roi (image_id, frame, kind, points, name, value, unit, basis, date_created, date_modified, def_key)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)",
                    params![image_id, frame, r.kind, points, r.name, r.value, r.unit, r.basis, now_unix(), r.def_key],
                )?;
                r.id = tx.last_insert_rowid();
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every named measurement in a study, for reports.
    pub fn named_measurements_for_study(&self, study_id: i64) -> Result<Vec<MeasurementRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.image_id, r.frame, r.kind, r.points, r.name, r.value, r.unit, r.basis,
                    i.series_id, s.modality, s.series_number, s.series_description, i.instance_number, r.def_key
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND r.name IS NOT NULL AND r.kind <> 'text' ORDER BY s.series_number, i.instance_number, r.id",
        )?;
        let rows = stmt.query_map(params![study_id], |row| {
            let modality: String = row.get(10)?;
            let number: Option<i64> = row.get(11)?;
            let description: String = row.get(12)?;
            let mut label = modality;
            if let Some(n) = number {
                label.push_str(&format!(" {n}"));
            }
            if !description.is_empty() {
                label.push_str(&format!(" {description}"));
            }
            Ok(MeasurementRow {
                roi: roi_from_row(row)?,
                series_id: row.get(9)?,
                series_label: label,
                instance_number: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every measurement of a study that has a value, or values worked out
    /// from it, with where it was taken: for exporting as data.
    pub fn export_rois(&self, study_id: i64) -> Result<Vec<crate::export_data::ExportedRoi>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.kind, r.name, r.def_key, r.value, r.unit, r.basis, r.frame, i.instance_number,
                    s.modality, s.series_number, s.series_description, s.series_instance_uid, i.sop_instance_uid
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND r.kind <> 'text'
               AND (r.value IS NOT NULL OR EXISTS (SELECT 1 FROM roi_value v WHERE v.roi_id = r.id))
             ORDER BY s.series_number, s.id, i.instance_number, r.frame, r.id",
        )?;
        let rows = stmt.query_map(params![study_id], |row| {
            let series_uid: String = row.get(12)?;
            Ok(crate::export_data::ExportedRoi {
                roi_id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                def_key: row.get(3)?,
                value: row.get(4)?,
                unit: row.get(5)?,
                basis: row.get(6)?,
                frame: row.get(7)?,
                instance_number: row.get(8)?,
                modality: row.get(9)?,
                series_number: row.get(10)?,
                series_description: row.get(11)?,
                // A loop has a series of its own here: give the file's series.
                series_uid: series_uid.split('#').next().unwrap_or_default().to_string(),
                sop_instance_uid: row.get(13)?,
                derived: Vec::new(),
            })
        })?;
        let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let mut values =
            conn.prepare("SELECT def_key, value, unit, basis FROM roi_value WHERE roi_id = ?1 ORDER BY def_key")?;
        for roi in &mut out {
            let rows = values.query_map(params![roi.roi_id], |row| {
                Ok(RoiValue {
                    def_key: row.get(0)?,
                    value: row.get(1)?,
                    unit: row.get(2)?,
                    basis: row.get(3)?,
                })
            })?;
            roi.derived = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        }
        Ok(out)
    }

    /// Every measurement of a study, image by image.
    pub fn rois_for_study(&self, study_id: i64) -> Result<Vec<RoiRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.image_id, r.frame, r.kind, r.points, r.name, r.value, r.unit, r.basis, r.def_key
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 ORDER BY s.series_number, s.id, i.instance_number, r.frame, r.id",
        )?;
        let rows = stmt.query_map(params![study_id], roi_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Whether a structured report or key image document has been turned
    /// into measurements and key images, or was written here.
    pub fn document_applied(&self, sop_instance_uid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM applied_document WHERE sop_instance_uid = ?1",
                params![sop_instance_uid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_document_applied(&self, sop_instance_uid: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR IGNORE INTO applied_document (sop_instance_uid, date_applied) VALUES (?1, ?2)",
            params![sop_instance_uid, now_unix()],
        )?;
        Ok(())
    }

    /// Keep what was cut in a series's 3D view, or forget it when nothing is.
    pub fn save_cuts(&self, series_id: i64, cuts: &crate::cuts::SavedCuts) -> Result<()> {
        let conn = self.conn.lock();
        if cuts.is_empty() {
            conn.execute("DELETE FROM cuts WHERE series_id = ?1", params![series_id])?;
            return Ok(());
        }
        let dims = serde_json::to_string(&cuts.dims).unwrap_or_default();
        let clip = cuts.clip.map(|c| serde_json::to_string(&c).unwrap_or_default());
        conn.execute(
            "INSERT OR REPLACE INTO cuts (series_id, dims, count, clip, mask, date_modified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                series_id,
                dims,
                cuts.count as i64,
                clip,
                crate::cuts::pack(&cuts.mask),
                now_unix()
            ],
        )?;
        Ok(())
    }

    /// What was cut in a series's 3D view, if it was on a grid of these dims.
    pub fn load_cuts(&self, series_id: i64, dims: [usize; 3]) -> Result<Option<crate::cuts::SavedCuts>> {
        let conn = self.conn.lock();
        let row: Option<(String, i64, Option<String>, Vec<u8>)> = conn
            .query_row(
                "SELECT dims, count, clip, mask FROM cuts WHERE series_id = ?1",
                params![series_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((saved_dims, count, clip, packed)) = row else {
            return Ok(None);
        };
        if serde_json::from_str::<[usize; 3]>(&saved_dims).ok() != Some(dims) {
            return Ok(None);
        }
        let Some(mask) = crate::cuts::unpack(&packed, dims.iter().product()) else {
            return Ok(None);
        };
        Ok(Some(crate::cuts::SavedCuts {
            dims,
            mask,
            count: count.clamp(0, 254) as u8,
            clip: clip.and_then(|c| serde_json::from_str(&c).ok()),
        }))
    }

    /// Give stored measurements their definition key from their name:
    /// a row named "LVIDd" without a key gets `lv.lvidd`. Returns how many
    /// rows changed.
    pub fn assign_def_keys_by_label(&self, label_keys: &[(String, String)]) -> Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let mut n = 0;
        for (label, key) in label_keys {
            n += tx.execute(
                "UPDATE roi SET def_key = ?1 WHERE def_key IS NULL AND name = ?2 COLLATE NOCASE",
                params![key, label],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Every valued measurement of a study keyed by definition: rows with
    /// a definition key, plus derived values (`roi_value`). Oldest edit
    /// first, so the last of a key is the latest.
    pub fn measurement_inputs_for_study(&self, study_id: i64) -> Result<Vec<MeasurementInput>> {
        let conn = self.conn.lock();
        let mut out = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT r.def_key, r.value, r.unit, r.basis, COALESCE(r.date_modified, 0)
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND r.def_key IS NOT NULL AND r.value IS NOT NULL
             ORDER BY COALESCE(r.date_modified, 0), r.id",
        )?;
        let rows = stmt.query_map(params![study_id], |row| {
            Ok(MeasurementInput {
                def_key: row.get(0)?,
                value: row.get(1)?,
                unit: row.get(2)?,
                basis: row.get(3)?,
                date_modified: row.get(4)?,
            })
        })?;
        out.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
        let mut stmt = conn.prepare(
            "SELECT v.def_key, v.value, v.unit, v.basis, COALESCE(r.date_modified, 0)
             FROM roi_value v JOIN roi r ON r.id = v.roi_id JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND v.value IS NOT NULL
             ORDER BY COALESCE(r.date_modified, 0), r.id, v.def_key",
        )?;
        let rows = stmt.query_map(params![study_id], |row| {
            Ok(MeasurementInput {
                def_key: row.get(0)?,
                value: row.get(1)?,
                unit: row.get(2)?,
                basis: row.get(3)?,
                date_modified: row.get(4)?,
            })
        })?;
        out.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
        out.sort_by_key(|m| m.date_modified);
        Ok(out)
    }

    /// Replace the derived values of a measurement.
    pub fn set_roi_values(&self, roi_id: i64, values: &[RoiValue]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM roi_value WHERE roi_id = ?1", params![roi_id])?;
        for v in values {
            tx.execute(
                "INSERT INTO roi_value (roi_id, def_key, value, unit, basis) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![roi_id, v.def_key, v.value, v.unit, v.basis],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn roi_values(&self, roi_id: i64) -> Result<Vec<RoiValue>> {
        let conn = self.conn.lock();
        Self::roi_values_in(&conn, roi_id)
    }

    fn roi_values_in(conn: &Connection, roi_id: i64) -> Result<Vec<RoiValue>> {
        let mut stmt =
            conn.prepare("SELECT def_key, value, unit, basis FROM roi_value WHERE roi_id = ?1 ORDER BY def_key")?;
        let rows = stmt.query_map(params![roi_id], |r| {
            Ok(RoiValue {
                def_key: r.get(0)?,
                value: r.get(1)?,
                unit: r.get(2)?,
                basis: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every LV M-mode line of a study with its values, oldest edit first.
    pub fn mmode_rows_for_study(&self, study_id: i64) -> Result<Vec<MmodeRow>> {
        self.mmode_rows("s.study_id = ?1", study_id)
    }

    /// Every LV M-mode line of a series with its values, oldest edit first.
    pub fn mmode_rows_for_series(&self, series_id: i64) -> Result<Vec<MmodeRow>> {
        self.mmode_rows("s.id = ?1", series_id)
    }

    fn mmode_rows(&self, filter: &str, id: i64) -> Result<Vec<MmodeRow>> {
        let conn = self.conn.lock();
        let sql = format!(
            "SELECT r.id, r.image_id, i.series_id, s.modality, s.series_number, s.series_description, i.instance_number,
                    r.kind, COALESCE(r.date_modified, 0), r.frame, r.points
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE {filter} AND (r.kind LIKE 'lvmmode-%' OR r.kind LIKE 'lv2d-%')
             ORDER BY COALESCE(r.date_modified, 0), r.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![id], |row| {
            let modality: String = row.get(3)?;
            let number: Option<i64> = row.get(4)?;
            let description: String = row.get(5)?;
            let mut label = modality;
            if let Some(n) = number {
                label.push_str(&format!(" {n}"));
            }
            if !description.is_empty() {
                label.push_str(&format!(" {description}"));
            }
            let kind: String = row.get(7)?;
            Ok(MmodeRow {
                roi_id: row.get(0)?,
                image_id: row.get(1)?,
                series_id: row.get(2)?,
                series_label: label,
                instance_number: row.get(6)?,
                phase: if kind.ends_with("-s") { 's' } else { 'd' },
                date_modified: row.get(8)?,
                values: Vec::new(),
                frame: row.get(9)?,
                x: serde_json::from_str::<Vec<[f32; 2]>>(&row.get::<_, String>(10)?)
                    .ok()
                    .and_then(|p| p.first().map(|q| q[0]))
                    .unwrap_or(0.0),
                two_d: kind.starts_with("lv2d-"),
            })
        })?;
        let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for r in &mut out {
            r.values = Self::roi_values_in(&conn, r.roi_id)?;
        }
        Ok(out)
    }

    /// The local study with this Study Instance UID, if it is in the archive.
    pub fn study_id_for_uid(&self, study_uid: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT id FROM study WHERE study_instance_uid = ?1",
                params![study_uid],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    /// What routing needs to know about newly imported images: their
    /// study, its UID and label, the series modality and the stored path.
    pub fn route_items(&self, image_ids: &[i64]) -> Result<Vec<crate::routing::RouteItem>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT i.id, st.id, st.study_instance_uid, st.patient_name, st.study_date, se.modality, i.path
             FROM image i JOIN series se ON se.id = i.series_id JOIN study st ON st.id = se.study_id
             WHERE i.id = ?1",
        )?;
        let mut out = Vec::new();
        for id in image_ids {
            let row = stmt
                .query_row(params![id], |r| {
                    Ok(crate::routing::RouteItem {
                        image_id: r.get(0)?,
                        study_id: r.get(1)?,
                        study_uid: r.get(2)?,
                        study_label: {
                            let name: String = r.get(3)?;
                            let date: Option<String> = r.get(4)?;
                            match date {
                                Some(d) if !d.is_empty() => format!("{name}, {d}"),
                                _ => name,
                            }
                        },
                        modality: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        path: r.get(6)?,
                    })
                })
                .optional()?;
            out.extend(row);
        }
        Ok(out)
    }

    /// Whether the archive holds a series with this Series Instance UID.
    pub fn series_in_archive(&self, series_uid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM series WHERE series_instance_uid = ?1",
                params![series_uid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Every image of a study, for sending it.
    pub fn images_for_study(&self, study_id: i64) -> Result<Vec<ImageRow>> {
        let mut out = Vec::new();
        for series in self.series_for_study(study_id)? {
            out.extend(self.images_for_series(series.id)?);
        }
        Ok(out)
    }

    /// Key images of a study in series and instance order.
    pub fn key_images_for_study(&self, study_id: i64) -> Result<Vec<ImageRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT i.id, i.series_id, i.sop_instance_uid, i.sop_class_uid, i.instance_number, i.number_of_frames,
                    i.rows, i.columns, i.path, i.stored_in_database_folder, i.file_size, i.transfer_syntax,
                    i.is_key_image, i.slice_location, i.comment, i.date_added
             FROM image i JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND i.is_key_image = 1
             ORDER BY s.series_number, i.instance_number, i.id",
        )?;
        let rows = stmt.query_map(params![study_id], image_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---------------------------------------------------------------- reports

    pub fn report(&self, study_id: i64) -> Result<Option<ReportRow>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT study_id, findings, conclusion, date_modified FROM report WHERE study_id = ?1",
                params![study_id],
                |r| {
                    Ok(ReportRow {
                        study_id: r.get(0)?,
                        findings: r.get(1)?,
                        conclusion: r.get(2)?,
                        date_modified: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_report(&self, study_id: i64, findings: &str, conclusion: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO report (study_id, findings, conclusion, date_modified) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(study_id) DO UPDATE SET findings = excluded.findings, conclusion = excluded.conclusion,
             date_modified = excluded.date_modified",
            params![study_id, findings, conclusion, now_unix()],
        )?;
        Ok(())
    }

    // ----------------------------------------------------------------- images

    pub fn images_for_series(&self, series_id: i64) -> Result<Vec<ImageRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, series_id, sop_instance_uid, sop_class_uid, instance_number, number_of_frames,
                    rows, columns, path, stored_in_database_folder, file_size, transfer_syntax,
                    is_key_image, slice_location, comment, date_added
             FROM image WHERE series_id = ?1
             ORDER BY instance_number, slice_location, id",
        )?;
        let rows = stmt.query_map(params![series_id], image_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every image of a study in the order it was taken.
    pub fn timeline(&self, study_id: i64) -> Result<Vec<TimelineEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT i.id, i.series_id, i.sop_instance_uid, i.sop_class_uid, i.instance_number, i.number_of_frames,
                    i.rows, i.columns, i.path, i.stored_in_database_folder, i.file_size, i.transfer_syntax,
                    i.is_key_image, i.slice_location, i.comment, i.date_added, i.acquired
             FROM image i JOIN series s ON s.id = i.series_id WHERE s.study_id = ?1",
        )?;
        let rows = stmt.query_map(params![study_id], |r| {
            Ok(TimelineEntry {
                image: image_from_row(r)?,
                acquired: r.get::<_, Option<String>>(16)?.filter(|t| !t.is_empty()),
            })
        })?;
        let mut entries = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        timeline_order(&mut entries);
        Ok(entries)
    }

    /// Images of a study whose time has not been read from their files yet.
    pub fn images_without_time(&self, study_id: i64) -> Result<Vec<ImageRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT i.id, i.series_id, i.sop_instance_uid, i.sop_class_uid, i.instance_number, i.number_of_frames,
                    i.rows, i.columns, i.path, i.stored_in_database_folder, i.file_size, i.transfer_syntax,
                    i.is_key_image, i.slice_location, i.comment, i.date_added
             FROM image i JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND i.acquired IS NULL",
        )?;
        let rows = stmt.query_map(params![study_id], image_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record an image's time. Empty means its file has none.
    pub fn set_image_time(&self, image_id: i64, acquired: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE image SET acquired = ?1 WHERE id = ?2",
            params![acquired, image_id],
        )?;
        Ok(())
    }

    pub fn image(&self, id: i64) -> Result<Option<ImageRow>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT id, series_id, sop_instance_uid, sop_class_uid, instance_number, number_of_frames,
                        rows, columns, path, stored_in_database_folder, file_size, transfer_syntax,
                        is_key_image, slice_location, comment, date_added
                 FROM image WHERE id = ?1",
                params![id],
                image_from_row,
            )
            .optional()?)
    }

    /// Study ids that have a report with any text, for the Report column.
    pub fn studies_with_reports(&self) -> Result<Vec<i64>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT study_id FROM report WHERE findings <> '' OR conclusion <> ''")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The image Horos uses for a series thumbnail: the middle one.
    pub fn representative_image(&self, series_id: i64) -> Result<Option<ImageRow>> {
        let images = self.images_for_series(series_id)?;
        if images.is_empty() {
            return Ok(None);
        }
        Ok(Some(images[images.len() / 2].clone()))
    }

    pub fn set_key_image(&self, image_id: i64, key: bool) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE image SET is_key_image = ?1 WHERE id = ?2",
            params![key, image_id],
        )?;
        Ok(())
    }

    pub fn image_path_exists(&self, path: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM image WHERE path = ?1", params![path], |r| {
            r.get(0)
        })?;
        Ok(n > 0)
    }

    // ------------------------------------------------------------ calibration

    /// User calibration (row mm, column mm) for an image, overriding the header.
    pub fn calibration(&self, image_id: i64) -> Result<Option<[f64; 2]>> {
        Ok(self.calibration_scoped(image_id)?.map(|(s, _)| s))
    }

    /// The calibration with the image point it was set from, when scoped.
    pub fn calibration_scoped(&self, image_id: i64) -> Result<Option<ScopedCalibration>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT row_mm, col_mm, scope_x, scope_y FROM calibration WHERE image_id = ?1",
                params![image_id],
                |r| {
                    let scope = match (r.get::<_, Option<f64>>(2)?, r.get::<_, Option<f64>>(3)?) {
                        (Some(x), Some(y)) => Some((x, y)),
                        _ => None,
                    };
                    Ok(([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?], scope))
                },
            )
            .optional()?)
    }

    pub fn set_calibration(&self, image_id: i64, row_mm: f64, col_mm: f64, description: &str) -> Result<()> {
        self.set_calibration_scoped(image_id, row_mm, col_mm, description, None)
    }

    /// Store a calibration, optionally scoped to the ultrasound region under
    /// the point it was set from.
    pub fn set_calibration_scoped(
        &self,
        image_id: i64,
        row_mm: f64,
        col_mm: f64,
        description: &str,
        scope: Option<(f64, f64)>,
    ) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO calibration (image_id, row_mm, col_mm, description, date_set, scope_x, scope_y)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                image_id,
                row_mm,
                col_mm,
                description,
                now_unix(),
                scope.map(|s| s.0),
                scope.map(|s| s.1)
            ],
        )?;
        Ok(())
    }

    /// Apply one calibration to every image of a series.
    pub fn set_series_calibration(&self, series_id: i64, row_mm: f64, col_mm: f64, description: &str) -> Result<usize> {
        self.set_series_calibration_scoped(series_id, row_mm, col_mm, description, None)
    }

    pub fn set_series_calibration_scoped(
        &self,
        series_id: i64,
        row_mm: f64,
        col_mm: f64,
        description: &str,
        scope: Option<(f64, f64)>,
    ) -> Result<usize> {
        let images = self.images_for_series(series_id)?;
        for img in &images {
            self.set_calibration_scoped(img.id, row_mm, col_mm, description, scope)?;
        }
        Ok(images.len())
    }

    pub fn clear_calibration(&self, image_id: i64) -> Result<()> {
        self.conn
            .lock()
            .execute("DELETE FROM calibration WHERE image_id = ?1", params![image_id])?;
        Ok(())
    }

    // ----------------------------------------------------------------- albums

    pub fn albums(&self) -> Result<Vec<AlbumRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, a.is_smart, a.predicate, a.idx,
                    (SELECT COUNT(*) FROM album_study als WHERE als.album_id = a.id)
             FROM album a ORDER BY a.idx, a.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AlbumRow {
                id: r.get(0)?,
                name: r.get(1)?,
                is_smart: r.get(2)?,
                predicate: r.get(3)?,
                index: r.get(4)?,
                number_of_studies: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn create_album(&self, name: &str, smart_filter: Option<&StudyFilter>) -> Result<i64> {
        let conn = self.conn.lock();
        let predicate = smart_filter.map(|f| serde_json::to_string(f).unwrap_or_default());
        let idx: i64 = conn.query_row("SELECT IFNULL(MAX(idx), 0) + 1 FROM album", [], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO album (name, is_smart, predicate, idx) VALUES (?1, ?2, ?3, ?4)",
            params![name, smart_filter.is_some(), predicate, idx],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn rename_album(&self, id: i64, name: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("UPDATE album SET name = ?1 WHERE id = ?2", params![name, id])?;
        Ok(())
    }

    pub fn delete_album(&self, id: i64) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM album_study WHERE album_id = ?1", params![id])?;
        tx.execute("DELETE FROM album WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_study_to_album(&self, album_id: i64, study_id: i64) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR IGNORE INTO album_study (album_id, study_id) VALUES (?1, ?2)",
            params![album_id, study_id],
        )?;
        Ok(())
    }

    pub fn remove_study_from_album(&self, album_id: i64, study_id: i64) -> Result<()> {
        self.conn.lock().execute(
            "DELETE FROM album_study WHERE album_id = ?1 AND study_id = ?2",
            params![album_id, study_id],
        )?;
        Ok(())
    }

    /// Smart album filter, decoded from its stored predicate.
    pub fn smart_album_filter(album: &AlbumRow) -> Option<StudyFilter> {
        if !album.is_smart {
            return None;
        }
        album
            .predicate
            .as_deref()
            .and_then(|p| serde_json::from_str::<StudyFilter>(p).ok())
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn merge_modality(existing: &str, new: &str) -> String {
    let new = new.trim();
    if new.is_empty() {
        return existing.to_string();
    }
    let mut parts: Vec<&str> = existing.split('\\').filter(|s| !s.is_empty()).collect();
    if !parts.contains(&new) {
        parts.push(new);
    }
    parts.join("\\")
}

fn study_from_row(r: &Row<'_>) -> rusqlite::Result<StudyRow> {
    Ok(StudyRow {
        id: r.get(0)?,
        study_instance_uid: r.get(1)?,
        patient_name: r.get(2)?,
        patient_id: r.get(3)?,
        patient_birth_date: r.get(4)?,
        patient_sex: r.get(5)?,
        patient_species: r.get(6)?,
        patient_breed: r.get(7)?,
        responsible_person: r.get(8)?,
        study_date: r.get(9)?,
        study_time: r.get(10)?,
        study_description: r.get(11)?,
        accession_number: r.get(12)?,
        referring_physician: r.get(13)?,
        institution_name: r.get(14)?,
        modalities: r.get(15)?,
        date_added: r.get(16)?,
        date_opened: r.get(17)?,
        comment: r.get(18)?,
        state: r.get(19)?,
        number_of_series: r.get(20)?,
        number_of_images: r.get(21)?,
        teaching_reference: r.get::<_, Option<String>>(22)?.filter(|v| !v.is_empty()),
        teaching_answer_key: r.get::<_, Option<i64>>(23)?.unwrap_or(0) != 0,
    })
}

fn series_from_row(r: &Row<'_>) -> rusqlite::Result<SeriesRow> {
    Ok(SeriesRow {
        id: r.get(0)?,
        study_id: r.get(1)?,
        series_instance_uid: r.get(2)?,
        series_number: r.get(3)?,
        modality: r.get(4)?,
        series_description: r.get(5)?,
        series_date: r.get(6)?,
        body_part_examined: r.get(7)?,
        view_position: r.get(8)?,
        laterality: r.get(9)?,
        number_of_images: r.get(10)?,
        number_of_key_images: r.get(11)?,
        window_center: r.get(12)?,
        window_width: r.get(13)?,
        rotation_angle: r.get(14)?,
        x_flipped: r.get(15)?,
        y_flipped: r.get(16)?,
        comment: r.get(17)?,
        date_added: r.get(18)?,
        number_of_frames: r.get(19)?,
    })
}

fn image_from_row(r: &Row<'_>) -> rusqlite::Result<ImageRow> {
    Ok(ImageRow {
        id: r.get(0)?,
        series_id: r.get(1)?,
        sop_instance_uid: r.get(2)?,
        sop_class_uid: r.get(3)?,
        instance_number: r.get(4)?,
        number_of_frames: r.get(5)?,
        rows: r.get(6)?,
        columns: r.get(7)?,
        path: r.get(8)?,
        stored_in_database_folder: r.get(9)?,
        file_size: r.get(10)?,
        transfer_syntax: r.get(11)?,
        is_key_image: r.get(12)?,
        slice_location: r.get(13)?,
        comment: r.get(14)?,
        date_added: r.get(15)?,
    })
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<_, _>>()?;
    if !existing.iter().any(|c| c == column) {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

fn teaching_case_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TeachingCase> {
    let blob: Vec<u8> = r.get(4)?;
    let mut secret = [0u8; 32];
    for (i, b) in blob.iter().take(32).enumerate() {
        secret[i] = *b;
    }
    Ok(TeachingCase {
        id: r.get(0)?,
        reference: r.get(1)?,
        name: r.get(2)?,
        title: r.get(3)?,
        secret,
        offset_days: r.get(5)?,
        preserve_season: r.get::<_, i64>(6)? != 0,
        note: r.get(7)?,
        created: r.get(8)?,
    })
}

fn roi_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoiRecord> {
    let points: String = row.get(4)?;
    Ok(RoiRecord {
        id: row.get(0)?,
        image_id: row.get(1)?,
        frame: row.get(2)?,
        kind: row.get(3)?,
        points: serde_json::from_str(&points).unwrap_or_default(),
        name: row.get(5)?,
        value: row.get(6)?,
        unit: row.get(7)?,
        basis: row.get::<_, Option<String>>(8)?,
        def_key: row.get::<_, Option<String>>("def_key")?,
    })
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

         CREATE TABLE IF NOT EXISTS study (
            id INTEGER PRIMARY KEY,
            study_instance_uid TEXT NOT NULL UNIQUE,
            patient_name TEXT NOT NULL DEFAULT '',
            patient_name_raw TEXT NOT NULL DEFAULT '',
            patient_id TEXT NOT NULL DEFAULT '',
            patient_birth_date TEXT,
            patient_sex TEXT NOT NULL DEFAULT '',
            patient_species TEXT,
            patient_breed TEXT,
            responsible_person TEXT,
            study_date TEXT,
            study_time TEXT,
            study_description TEXT NOT NULL DEFAULT '',
            accession_number TEXT NOT NULL DEFAULT '',
            referring_physician TEXT NOT NULL DEFAULT '',
            institution_name TEXT NOT NULL DEFAULT '',
            study_id_tag TEXT NOT NULL DEFAULT '',
            modalities TEXT NOT NULL DEFAULT '',
            date_added INTEGER NOT NULL,
            date_opened INTEGER,
            comment TEXT NOT NULL DEFAULT '',
            state INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_study_patient_name ON study(patient_name COLLATE NOCASE);
         CREATE INDEX IF NOT EXISTS idx_study_patient_id ON study(patient_id);
         CREATE INDEX IF NOT EXISTS idx_study_date ON study(study_date);
         CREATE INDEX IF NOT EXISTS idx_study_date_added ON study(date_added);

         CREATE TABLE IF NOT EXISTS series (
            id INTEGER PRIMARY KEY,
            study_id INTEGER NOT NULL REFERENCES study(id) ON DELETE CASCADE,
            series_instance_uid TEXT NOT NULL UNIQUE,
            series_number INTEGER,
            modality TEXT NOT NULL DEFAULT '',
            series_description TEXT NOT NULL DEFAULT '',
            series_date TEXT,
            body_part_examined TEXT NOT NULL DEFAULT '',
            view_position TEXT NOT NULL DEFAULT '',
            laterality TEXT NOT NULL DEFAULT '',
            window_center REAL,
            window_width REAL,
            rotation_angle REAL NOT NULL DEFAULT 0,
            x_flipped INTEGER NOT NULL DEFAULT 0,
            y_flipped INTEGER NOT NULL DEFAULT 0,
            comment TEXT NOT NULL DEFAULT '',
            date_added INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_series_study ON series(study_id);

         CREATE TABLE IF NOT EXISTS image (
            id INTEGER PRIMARY KEY,
            series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
            sop_instance_uid TEXT NOT NULL UNIQUE,
            sop_class_uid TEXT NOT NULL DEFAULT '',
            instance_number INTEGER,
            number_of_frames INTEGER NOT NULL DEFAULT 1,
            rows INTEGER NOT NULL DEFAULT 0,
            columns INTEGER NOT NULL DEFAULT 0,
            path TEXT NOT NULL,
            stored_in_database_folder INTEGER NOT NULL DEFAULT 1,
            file_size INTEGER NOT NULL DEFAULT 0,
            transfer_syntax TEXT NOT NULL DEFAULT '',
            is_key_image INTEGER NOT NULL DEFAULT 0,
            slice_location REAL,
            comment TEXT NOT NULL DEFAULT '',
            date_added INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_image_series ON image(series_id);

         CREATE TABLE IF NOT EXISTS album (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            is_smart INTEGER NOT NULL DEFAULT 0,
            predicate TEXT,
            idx INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS album_study (
            album_id INTEGER NOT NULL REFERENCES album(id) ON DELETE CASCADE,
            study_id INTEGER NOT NULL REFERENCES study(id) ON DELETE CASCADE,
            PRIMARY KEY (album_id, study_id)
         );

         CREATE TABLE IF NOT EXISTS applied_document (
            sop_instance_uid TEXT PRIMARY KEY,
            date_applied INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS cuts (
            series_id INTEGER PRIMARY KEY REFERENCES series(id) ON DELETE CASCADE,
            dims TEXT NOT NULL,
            count INTEGER NOT NULL,
            clip TEXT,
            mask BLOB NOT NULL,
            date_modified INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS calibration (
            image_id INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
            row_mm REAL NOT NULL,
            col_mm REAL NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            date_set INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS roi (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            image_id INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
            frame INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL,
            points TEXT NOT NULL,
            name TEXT,
            value REAL,
            unit TEXT NOT NULL DEFAULT '',
            date_created INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS roi_image ON roi(image_id, frame);
         CREATE TABLE IF NOT EXISTS report (
            study_id INTEGER PRIMARY KEY REFERENCES study(id) ON DELETE CASCADE,
            findings TEXT NOT NULL DEFAULT '',
            conclusion TEXT NOT NULL DEFAULT '',
            date_modified INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS teaching_case (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reference TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL UNIQUE,
            title TEXT NOT NULL DEFAULT '',
            secret BLOB NOT NULL,
            offset_days INTEGER NOT NULL,
            preserve_season INTEGER NOT NULL DEFAULT 0,
            note TEXT NOT NULL DEFAULT '',
            created INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS teaching_source (
            case_id INTEGER NOT NULL REFERENCES teaching_case(id) ON DELETE CASCADE,
            study_instance_uid TEXT NOT NULL,
            study_id INTEGER,
            PRIMARY KEY (case_id, study_instance_uid)
         );
         CREATE TABLE IF NOT EXISTS teaching_export (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            case_id INTEGER NOT NULL REFERENCES teaching_case(id) ON DELETE CASCADE,
            study_instance_uid TEXT NOT NULL,
            time INTEGER NOT NULL,
            destination TEXT NOT NULL,
            result TEXT NOT NULL,
            series INTEGER NOT NULL DEFAULT 0,
            images INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    // Columns added after the first schema; SQLite has no IF NOT EXISTS for them.
    add_column_if_missing(conn, "study", "teaching_reference", "TEXT")?;
    add_column_if_missing(conn, "study", "teaching_answer_key", "INTEGER NOT NULL DEFAULT 0")?;
    // When an image was taken: NULL until looked for, empty if the file has none.
    add_column_if_missing(conn, "image", "acquired", "TEXT")?;
    add_column_if_missing(conn, "roi", "concept_code", "TEXT")?;
    add_column_if_missing(conn, "roi", "basis", "TEXT")?;
    add_column_if_missing(conn, "roi", "def_key", "TEXT")?;
    add_column_if_missing(conn, "roi", "date_modified", "INTEGER")?;
    add_column_if_missing(conn, "calibration", "scope_x", "REAL")?;
    add_column_if_missing(conn, "calibration", "scope_y", "REAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS catalogue (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            root TEXT NOT NULL UNIQUE,
            scanned_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS watched_file (
            path TEXT PRIMARY KEY,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS roi_value (
            roi_id INTEGER NOT NULL REFERENCES roi(id) ON DELETE CASCADE,
            def_key TEXT NOT NULL,
            value REAL,
            unit TEXT NOT NULL DEFAULT '',
            basis TEXT,
            PRIMARY KEY (roi_id, def_key)
         );",
    )?;
    // Version 3: relative paths are stored with forward slashes on every
    // platform, so an archive moves between a PC and a Mac.
    let previous: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if previous < 3 {
        conn.execute(
            "UPDATE image SET path = REPLACE(path, '\\', '/') WHERE stored_in_database_folder = 1",
            [],
        )?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruthos_dicom::meta::PatientName;

    fn meta(study: &str, series: &str, sop: &str) -> InstanceMeta {
        InstanceMeta {
            patient_name: PatientName::parse("REX^FIDO"),
            patient_id: "V123".into(),
            study_instance_uid: study.into(),
            series_instance_uid: series.into(),
            sop_instance_uid: sop.into(),
            modality: "CR".into(),
            study_date: Some("20260101".into()),
            rows: 10,
            columns: 10,
            number_of_frames: 1,
            ..Default::default()
        }
    }

    #[test]
    fn insert_dedup_and_counts() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let a = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap();
        assert!(matches!(a, InsertOutcome::Inserted { new_study: true, .. }));
        let b = db
            .insert_instance(&meta("1", "1.1", "1.1.2"), "b.dcm", true, 10)
            .unwrap();
        assert!(matches!(b, InsertOutcome::Inserted { new_study: false, .. }));
        let c = db
            .insert_instance(&meta("1", "1.1", "1.1.2"), "b.dcm", true, 10)
            .unwrap();
        assert_eq!(c, InsertOutcome::Duplicate);
        let mut m = meta("1", "1.2", "1.2.1");
        m.modality = "DX".into();
        db.insert_instance(&m, "c.dcm", true, 10).unwrap();

        let studies = db.list_studies(&StudyFilter::default()).unwrap();
        assert_eq!(studies.len(), 1);
        assert_eq!(studies[0].number_of_series, 2);
        assert_eq!(studies[0].number_of_images, 3);
        assert_eq!(studies[0].modalities, "CR\\DX");
        assert_eq!(studies[0].patient_name, "REX, FIDO");
        assert_eq!(studies[0].study_date.as_deref(), Some("2026-01-01"));

        let f = StudyFilter {
            modality: "DX".into(),
            ..Default::default()
        };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);
        let f = StudyFilter {
            modality: "MR".into(),
            ..Default::default()
        };
        assert_eq!(db.list_studies(&f).unwrap().len(), 0);
        let f = StudyFilter {
            text: "fido".into(),
            ..Default::default()
        };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);
    }

    #[test]
    fn albums_and_delete() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { study_id, .. } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        let album = db.create_album("Hips", None).unwrap();
        db.add_study_to_album(album, study_id).unwrap();
        let albums = db.albums().unwrap();
        assert_eq!(albums[0].number_of_studies, 1);
        let f = StudyFilter {
            album_id: Some(album),
            ..Default::default()
        };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);

        let files = db.delete_study(study_id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(db.counts().unwrap(), (0, 0, 0));
        assert_eq!(db.albums().unwrap()[0].number_of_studies, 0);
    }

    #[test]
    fn multiframe_files_get_their_own_series() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let mut loop1 = meta("1", "1.1", "1.1.1");
        loop1.number_of_frames = 40;
        loop1.instance_number = Some(1);
        loop1.modality = "US".into();
        let mut loop2 = loop1.clone();
        loop2.sop_instance_uid = "1.1.2".into();
        loop2.instance_number = Some(2);
        let mut still = loop1.clone();
        still.sop_instance_uid = "1.1.3".into();
        still.number_of_frames = 1;
        still.instance_number = Some(3);
        let InsertOutcome::Inserted { study_id, .. } = db.insert_instance(&loop1, "a.dcm", true, 10).unwrap() else {
            panic!()
        };
        db.insert_instance(&loop2, "b.dcm", true, 10).unwrap();
        db.insert_instance(&still, "c.dcm", true, 10).unwrap();
        let series = db.series_for_study(study_id).unwrap();
        assert_eq!(series.len(), 3, "two loops and the still frame are three series");
        assert!(series.iter().any(|s| s.series_description.ends_with("loop 1")));
        assert!(series.iter().any(|s| s.series_description.ends_with("loop 2")));
        // The still image keeps the original series UID.
        assert!(series.iter().any(|s| s.series_instance_uid == "1.1"));
    }

    #[test]
    fn report_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { study_id, image_id, .. } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        assert!(db.report(study_id).unwrap().is_none());
        db.set_report(study_id, "findings", "conclusion").unwrap();
        db.set_report(study_id, "findings 2", "conclusion").unwrap();
        assert_eq!(db.report(study_id).unwrap().unwrap().findings, "findings 2");
        let rec = RoiRecord {
            id: 0,
            image_id,
            frame: 0,
            kind: "length".into(),
            points: vec![[0.0, 0.0], [3.0, 4.0]],
            name: Some("VHS long axis".into()),
            value: Some(50.0),
            unit: "mm".into(),
            basis: None,
            def_key: None,
        };
        db.replace_rois(image_id, 0, &[rec]).unwrap();
        let named = db.named_measurements_for_study(study_id).unwrap();
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].roi.name.as_deref(), Some("VHS long axis"));
        assert!(named[0].series_label.starts_with("CR"));
        db.set_key_image(image_id, true).unwrap();
        assert_eq!(db.key_images_for_study(study_id).unwrap().len(), 1);
    }

    #[test]
    fn rois_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { image_id, .. } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        assert!(db.rois(image_id, 0).unwrap().is_empty());
        let rec = RoiRecord {
            id: 0,
            image_id,
            frame: 0,
            kind: "length".into(),
            points: vec![[1.0, 2.0], [3.5, 4.0]],
            name: Some("LVIDd".into()),
            value: Some(42.0),
            unit: "mm".into(),
            basis: None,
            def_key: None,
        };
        db.replace_rois(image_id, 0, std::slice::from_ref(&rec)).unwrap();
        let got = db.rois(image_id, 0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].points, rec.points);
        assert_eq!(got[0].name.as_deref(), Some("LVIDd"));
        db.replace_rois(image_id, 0, &[]).unwrap();
        assert!(db.rois(image_id, 0).unwrap().is_empty());
    }

    #[test]
    fn measurements_export_as_rows_with_and_without_identity() {
        use crate::export_data::gather;
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let mut m = meta("1", "1.1", "1.1.1");
        m.patient_birth_date = Some("20200101".into());
        m.study_date = Some("20260101".into());
        m.accession_number = "ACC9".into();
        let InsertOutcome::Inserted { image_id, study_id, .. } = db.insert_instance(&m, "a.dcm", true, 10).unwrap()
        else {
            panic!()
        };
        let line = |key: &str, name: &str, value: f64| RoiRecord {
            id: 0,
            image_id,
            frame: 2,
            kind: "length".into(),
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            name: Some(name.into()),
            value: Some(value),
            unit: "mm".into(),
            basis: Some("ultrasound-region".into()),
            def_key: Some(key.into()),
        };
        let mut recs = vec![
            line("lv.lvidd", "LVIDd", 40.0),
            line("lv.lvids", "LVIDs", 30.0),
            RoiRecord {
                kind: "text".into(),
                value: None,
                ..line("", "=cmd|' /C calc'!A0", 0.0)
            },
        ];
        db.sync_rois(image_id, 2, &mut recs).unwrap();
        db.set_roi_values(
            recs[0].id,
            &[RoiValue {
                def_key: "lv.ivsd".into(),
                value: Some(9.5),
                unit: "mm".into(),
                basis: Some("ultrasound-region".into()),
            }],
        )
        .unwrap();
        db.set_report(study_id, "Normal, \"thin\" walls", "No change").unwrap();
        let library = crate::measure::builtin();

        let full = gather(&db, &library, &[study_id], true).unwrap();
        let values = &full.studies[0].values;
        let kinds: Vec<&str> = values.iter().map(|v| v.kind).collect();
        assert_eq!(kinds[..3], ["measurement", "derived", "measurement"], "{values:#?}");
        assert!(kinds.len() > 3 && kinds[3..].iter().all(|k| *k == "calculation"));
        let fs = values.iter().find(|v| v.key == "lv.fs").unwrap();
        assert!((fs.value.unwrap() - 25.0).abs() < 1e-9, "(40 - 30) / 40");
        assert_eq!(fs.inputs, "lv.lvidd x1; lv.lvids x1");
        assert_eq!(full.studies[0].patient.age_days, Some(2192));
        let csv = full.to_csv();
        assert!(csv.starts_with("\u{feff}case,patient_name,"));
        assert_eq!(
            csv.lines().count(),
            1 + values.len() + 2,
            "a header, the values and two report rows"
        );
        assert!(csv.contains("ACC9") && csv.contains("1.1.1") && csv.contains("\"Normal, \"\"thin\"\" walls\""));
        assert!(!csv.contains("cmd|"), "text notes are not values");
        let json: serde_json::Value = serde_json::from_str(&full.to_json()).unwrap();
        assert_eq!(json["studies"][0]["values"][0]["at"]["frame"], 2);
        assert_eq!(json["studies"][0]["report"]["conclusion"], "No change");

        let bare = gather(&db, &library, &[study_id], false).unwrap();
        let csv = bare.to_csv();
        for gone in [
            "REX",
            "V123",
            "ACC9",
            "2026-01-01",
            "2020-01-01",
            "1.1.1",
            "Normal",
            "No change",
        ] {
            assert!(!csv.contains(gone), "{gone} should be left out:\n{csv}");
        }
        assert!(csv.contains(",2192,"), "age in days stays");
        let json = bare.to_json();
        assert!(!json.contains("REX") && !json.contains("\"report\"") && !json.contains("sop_instance_uid"));
        assert_eq!(bare.value_count(), values.len());
    }

    /// Acceptance test 5 (storage half): editing a measurement's points keeps
    /// its row id, name and creation time.
    #[test]
    fn edited_rois_keep_their_row() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { image_id, .. } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        let mut recs = vec![RoiRecord {
            id: 0,
            image_id,
            frame: 0,
            kind: "rect".into(),
            points: vec![[1.0, 2.0], [3.5, 4.0]],
            name: Some("LA".into()),
            value: Some(42.0),
            unit: "mm2".into(),
            basis: Some("ultrasound-region".into()),
            def_key: None,
        }];
        db.sync_rois(image_id, 0, &mut recs).unwrap();
        let id = recs[0].id;
        assert!(id > 0, "sync fills in the new row's id");
        let created: i64 = db
            .conn
            .lock()
            .query_row("SELECT date_created FROM roi WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        // Move one corner: the row is updated in place.
        recs[0].points = vec![[1.0, 2.0], [6.0, 4.0]];
        recs[0].value = Some(50.0);
        db.sync_rois(image_id, 0, &mut recs).unwrap();
        let got = db.rois(image_id, 0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, id);
        assert_eq!(got[0].points, vec![[1.0, 2.0], [6.0, 4.0]]);
        assert_eq!(got[0].name.as_deref(), Some("LA"));
        // A definition key assigned from the label survives round trips.
        assert_eq!(
            db.assign_def_keys_by_label(&[("la".into(), "echo.la".into())]).unwrap(),
            1
        );
        assert_eq!(db.rois(image_id, 0).unwrap()[0].def_key.as_deref(), Some("echo.la"));
        recs[0].def_key = Some("echo.la".into());
        let created_after: i64 = db
            .conn
            .lock()
            .query_row("SELECT date_created FROM roi WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(created, created_after);
        // A second measurement added later gets its own id; the first keeps its row.
        recs.push(RoiRecord {
            id: 0,
            name: None,
            kind: "length".into(),
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            ..recs[0].clone()
        });
        db.sync_rois(image_id, 0, &mut recs).unwrap();
        assert_eq!(recs[0].id, id);
        assert!(recs[1].id > id);
        assert_eq!(db.rois(image_id, 0).unwrap().len(), 2);
    }

    /// Acceptance: an M-mode line stores three values and the study query
    /// returns them with the line's phase and edit time.
    #[test]
    fn mmode_values_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { image_id, .. } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        let study_id = db.list_studies(&StudyFilter::default()).unwrap()[0].id;
        let mut recs = vec![RoiRecord {
            id: 0,
            image_id,
            frame: 0,
            kind: "lvmmode-d".into(),
            points: vec![[100.0, 10.0], [100.0, 20.0], [100.0, 60.0], [100.0, 70.0]],
            name: None,
            value: Some(40.0),
            unit: "mm".into(),
            basis: Some("ultrasound-region".into()),
            def_key: None,
        }];
        db.sync_rois(image_id, 0, &mut recs).unwrap();
        let values: Vec<RoiValue> = crate::mmode::KEYS_D
            .iter()
            .zip([10.0, 40.0, 10.0])
            .map(|(k, v)| RoiValue {
                def_key: (*k).into(),
                value: Some(v),
                unit: "mm".into(),
                basis: Some("ultrasound-region".into()),
            })
            .collect();
        db.set_roi_values(recs[0].id, &values).unwrap();
        let rows = db.mmode_rows_for_study(study_id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].phase, 'd');
        assert_eq!(rows[0].value("lv.lvidd"), Some(40.0));
        assert_eq!(rows[0].values.len(), 3);
        // Deleting the line removes its values.
        db.sync_rois(image_id, 0, &mut []).unwrap();
        assert!(db.roi_values(recs[0].id).unwrap().is_empty());
    }

    /// An archive written on Windows before 0.1.0-alpha holds backslashes;
    /// opening it rewrites them so the same file resolves on a Mac.
    /// Save a study's measurements as a report, then bring it into a second
    /// archive, as a PACS or a colleague would.
    #[test]
    fn a_report_carries_the_measurements_to_another_archive() {
        let root = std::env::temp_dir().join(format!("ruthos-sr-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let library = crate::measure::builtin();
        let sample = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pydicom/CT_small.dcm");

        let here = DataPaths::new(root.join("here"));
        let db = Database::open_in_memory(&here).unwrap();
        let crate::import::FileOutcome::Imported { image_id, study_id, .. } =
            crate::import::import_file(&db, &here, &sample, crate::import::ImportMode::Copy).unwrap()
        else {
            panic!()
        };
        let mut recs = vec![
            RoiRecord {
                id: 0,
                image_id,
                frame: 0,
                kind: "length".into(),
                points: vec![[10.0, 10.0], [10.0, 50.0]],
                name: Some("LVIDd".into()),
                value: Some(40.0),
                unit: "mm".into(),
                basis: Some("pixel-spacing".into()),
                def_key: Some("lv.lvidd".into()),
            },
            RoiRecord {
                id: 0,
                image_id,
                frame: 0,
                kind: "length".into(),
                points: vec![[20.0, 10.0], [20.0, 40.0]],
                name: Some("LVIDs".into()),
                value: Some(30.0),
                unit: "mm".into(),
                basis: Some("pixel-spacing".into()),
                def_key: Some("lv.lvids".into()),
            },
        ];
        db.sync_rois(image_id, 0, &mut recs).unwrap();
        db.set_key_image(image_id, true).unwrap();
        db.set_report(study_id, "Walls normal.", "No change.").unwrap();

        let saved = crate::structured::save(&db, &here, &library, study_id).unwrap();
        assert!(saved.measurements.unwrap() >= 3, "two lines and a calculation");
        assert_eq!(saved.key_images, Some(1));
        // Ruthos's own documents are not applied again here.
        let again = crate::structured::apply(&db, &here, &library, study_id).unwrap();
        assert_eq!(again, crate::structured::Applied::default());
        assert_eq!(db.rois(image_id, 0).unwrap().len(), 2);

        // Another archive: the report arrives before the image, then the image.
        let there = DataPaths::new(root.join("there"));
        let other = Database::open_in_memory(&there).unwrap();
        let docs: Vec<_> = db
            .images_for_study(study_id)
            .unwrap()
            .into_iter()
            .filter(|i| i.id != image_id)
            .collect();
        assert_eq!(docs.len(), 2, "a report and a key image document");
        let mut other_study = 0;
        for d in &docs {
            let crate::import::FileOutcome::Imported { study_id, .. } =
                crate::import::import_file(&other, &there, &here.resolve(&d.path), crate::import::ImportMode::Copy)
                    .unwrap()
            else {
                panic!()
            };
            other_study = study_id;
        }
        let waiting = crate::structured::apply(&other, &there, &library, other_study).unwrap();
        assert_eq!(
            waiting,
            crate::structured::Applied::default(),
            "nothing to put them on yet"
        );
        let crate::import::FileOutcome::Imported { image_id: arrived, .. } =
            crate::import::import_file(&other, &there, &sample, crate::import::ImportMode::Copy).unwrap()
        else {
            panic!()
        };
        let applied = crate::structured::apply(&other, &there, &library, other_study).unwrap();
        assert_eq!(applied.measurements, 2);
        assert_eq!(applied.key_images, 1);
        let back = other.rois(arrived, 0).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].def_key.as_deref(), Some("lv.lvidd"));
        assert_eq!(back[0].points, vec![[10.0, 10.0], [10.0, 50.0]]);
        assert_eq!(back[0].basis.as_deref(), Some("pixel-spacing"));
        // Once only.
        let twice = crate::structured::apply(&other, &there, &library, other_study).unwrap();
        assert_eq!(twice, crate::structured::Applied::default());
        assert_eq!(other.rois(arrived, 0).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cuts_are_kept_with_the_series_and_go_with_it() {
        use crate::cuts::{SavedClip, SavedCuts};
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted {
            series_id, study_id, ..
        } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        let dims = [4, 5, 6];
        let mut mask = vec![0u8; 120];
        mask[10..20].fill(1);
        mask[50] = 2;
        let cuts = SavedCuts {
            dims,
            mask,
            count: 2,
            clip: Some(SavedClip {
                low: [0.0, 1.0, 2.0],
                high: [3.0, 4.0, 5.0],
                part: Some(3),
                removes: true,
            }),
        };
        db.save_cuts(series_id, &cuts).unwrap();
        assert_eq!(db.load_cuts(series_id, dims).unwrap(), Some(cuts.clone()));
        assert_eq!(
            db.load_cuts(series_id, [4, 5, 7]).unwrap(),
            None,
            "another grid gets none"
        );

        let none = SavedCuts {
            mask: vec![0; 120],
            count: 0,
            clip: None,
            ..cuts.clone()
        };
        db.save_cuts(series_id, &none).unwrap();
        assert_eq!(db.load_cuts(series_id, dims).unwrap(), None, "nothing cut is forgotten");

        db.save_cuts(series_id, &cuts).unwrap();
        db.delete_study(study_id).unwrap();
        let left: i64 = db
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM cuts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "deleting the study takes its cuts");
    }

    #[test]
    fn every_column_sorts_both_ways_with_blanks_last() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let add = |study: &str, name: &str, id: &str, date: &str, born: &str, description: &str, images: usize| {
            let mut m = meta(study, study, &format!("{study}.0"));
            m.patient_name = PatientName::parse(name);
            m.patient_id = id.into();
            m.study_date = (!date.is_empty()).then(|| date.to_string());
            m.patient_birth_date = (!born.is_empty()).then(|| born.to_string());
            m.study_description = description.into();
            db.insert_instance(&m, study, true, 10).unwrap();
            for n in 1..images {
                db.insert_instance(&meta(study, study, &format!("{study}.{n}")), study, true, 10)
                    .unwrap();
            }
        };
        // Age: 9 months, 2 years, 3 weeks, none. As text, "9M" would come last.
        add("a", "BETA", "2", "20260601", "20250901", "Echo", 1);
        add("b", "alpha", "10", "20260101", "20240101", "", 3);
        add("c", "GAMMA", "", "20261101", "20261010", "Abdomen", 2);
        add("d", "DELTA", "3", "", "", "Thorax", 4);

        let by = |column: StudyColumn, descending: bool| -> String {
            let filter = StudyFilter {
                order: Some(StudyOrder { column, descending }),
                ..Default::default()
            };
            let studies = db.list_studies(&filter).unwrap();
            studies.iter().map(|s| s.study_instance_uid.as_str()).collect()
        };
        assert_eq!(by(StudyColumn::PatientName, false), "badc");
        assert_eq!(by(StudyColumn::PatientName, true), "cdab");
        assert_eq!(by(StudyColumn::Age, false), "cabd", "youngest first, by real age");
        assert_eq!(by(StudyColumn::Age, true), "bacd", "no age stays last");
        assert_eq!(by(StudyColumn::Images, true), "dbca");
        assert_eq!(by(StudyColumn::StudyDate, true), "cabd", "no date stays last");
        assert_eq!(by(StudyColumn::StudyDate, false), "bacd", "no date stays last");
        assert_eq!(by(StudyColumn::Description, false), "cadb", "no description stays last");
        assert_eq!(by(StudyColumn::Description, true), "dacb", "no description stays last");
        assert_eq!(
            by(StudyColumn::PatientId, true),
            "dabc",
            "as text, and no ID stays last"
        );

        for column in [
            StudyColumn::Report,
            StudyColumn::Accession,
            StudyColumn::Modality,
            StudyColumn::DateAdded,
            StudyColumn::Comments,
            StudyColumn::Status,
        ] {
            assert_eq!(by(column, false).len(), 4, "{column:?}");
            assert_eq!(by(column, true).len(), 4, "{column:?}");
        }
    }

    #[test]
    fn clicking_a_heading_picks_a_way_then_reverses_it() {
        let start = StudyOrder::from(StudySort::DateAddedDesc);
        let name = start.after_click(StudyColumn::PatientName);
        assert!(!name.descending, "names start at A");
        assert!(name.after_click(StudyColumn::PatientName).descending);
        assert!(
            name.after_click(StudyColumn::StudyDate).descending,
            "dates start newest"
        );
        assert!(
            !start.after_click(StudyColumn::DateAdded).descending,
            "the same heading reverses"
        );
    }

    #[test]
    fn a_timeline_mixes_stills_and_loops_by_time() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let add = |sop: &str, n: i64, frames: u32, time: &str| {
            let mut m = meta("1", "1.1", sop);
            m.instance_number = Some(n);
            m.number_of_frames = frames;
            m.content_date = Some("20261105".into());
            m.content_time = Some(time.into());
            db.insert_instance(&m, sop, true, 10).unwrap();
        };
        add("s1", 1, 1, "100000");
        add("l1", 2, 40, "100010");
        add("s2", 3, 1, "100020");
        add("l2", 4, 40, "100030");
        let study = db.list_studies(&StudyFilter::default()).unwrap()[0].id;
        assert_eq!(
            db.series_for_study(study).unwrap().len(),
            3,
            "each loop is still its own series"
        );

        let timeline = db.timeline(study).unwrap();
        let order: Vec<&str> = timeline.iter().map(|e| e.image.path.as_str()).collect();
        assert_eq!(order, ["s1", "l1", "s2", "l2"]);
        assert_eq!(timeline[1].clock().as_deref(), Some("10:00:10"));
        assert_eq!(timeline[1].gap_since(&timeline[0]), None, "seconds apart is no gap");
        let at = |time: &str| TimelineEntry {
            acquired: Some(time.to_string()),
            ..timeline[0].clone()
        };
        let start = at("20261105100000.000000");
        assert_eq!(at("20261105101800.000000").gap_since(&start).unwrap(), "18 min later");
        assert_eq!(
            at("20261105121100.000000").gap_since(&start).unwrap(),
            "2 h 11 min later"
        );
        assert_eq!(at("20261108100000.000000").gap_since(&start).unwrap(), "3 days later");

        // An older archive: nothing read yet, then read from the files.
        db.conn.lock().execute("UPDATE image SET acquired = NULL", []).unwrap();
        assert_eq!(db.images_without_time(study).unwrap().len(), 4);
        let timeline = db.timeline(study).unwrap();
        let by_number: Vec<&str> = timeline.iter().map(|e| e.image.path.as_str()).collect();
        assert_eq!(by_number, ["s1", "l1", "s2", "l2"], "instance numbers stand in");
        for image in db.images_without_time(study).unwrap() {
            db.set_image_time(image.id, "").unwrap();
        }
        assert!(
            db.images_without_time(study).unwrap().is_empty(),
            "a file with no time is not read twice"
        );
    }

    #[test]
    fn stored_paths_migrate_to_forward_slashes() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        db.insert_instance(&meta("1", "1.1", "1.1.1"), "DATABASE.noindex\\ab\\cd\\1.dcm", true, 10)
            .unwrap();
        db.insert_instance(&meta("1", "1.1", "1.1.2"), "C:\\linked\\2.dcm", false, 10)
            .unwrap();
        {
            let conn = db.conn.lock();
            conn.execute("UPDATE meta SET value = '2' WHERE key = 'schema_version'", [])
                .unwrap();
            migrate(&conn).unwrap();
        }
        let paths: Vec<(String, bool)> = {
            let conn = db.conn.lock();
            let mut stmt = conn
                .prepare("SELECT path, stored_in_database_folder FROM image ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(paths[0], ("DATABASE.noindex/ab/cd/1.dcm".to_string(), true));
        assert_eq!(
            paths[1],
            ("C:\\linked\\2.dcm".to_string(), false),
            "linked absolute paths are left alone"
        );
    }

    #[test]
    fn calibration_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted {
            series_id, image_id, ..
        } = db
            .insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10)
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(db.calibration(image_id).unwrap(), None);
        db.set_calibration(image_id, 0.1, 0.1, "ruler").unwrap();
        assert_eq!(db.calibration(image_id).unwrap(), Some([0.1, 0.1]));
        assert_eq!(db.set_series_calibration(series_id, 0.2, 0.2, "ruler").unwrap(), 1);
        assert_eq!(db.calibration(image_id).unwrap(), Some([0.2, 0.2]));
        db.clear_calibration(image_id).unwrap();
        assert_eq!(db.calibration(image_id).unwrap(), None);
    }
}
