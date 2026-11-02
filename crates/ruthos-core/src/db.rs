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

use crate::model::{AlbumRow, DateFilter, ImageRow, SeriesRow, StudyRow, RoiRecord};
use crate::paths::DataPaths;
use crate::Result;

const SCHEMA_VERSION: i64 = 1;

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
                    tx.execute(
                        "UPDATE study SET modalities = ?1 WHERE id = ?2",
                        params![merged, id],
                    )?;
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

        // Series
        let existing_series: Option<i64> = tx
            .query_row(
                "SELECT id FROM series WHERE series_instance_uid = ?1",
                params![meta.series_instance_uid],
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
                        meta.series_instance_uid,
                        meta.series_number,
                        meta.modality,
                        meta.series_description,
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
                slice_location, comment, date_added)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, '', ?13)",
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
                    (SELECT COUNT(*) FROM image i JOIN series se ON i.series_id = se.id WHERE se.study_id = s.id)
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
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(match filter.sort {
            StudySort::DateAddedDesc => " ORDER BY s.date_added DESC, s.id DESC",
            StudySort::StudyDateDesc => " ORDER BY s.study_date DESC, s.study_time DESC, s.id DESC",
            StudySort::PatientName => " ORDER BY s.patient_name COLLATE NOCASE, s.study_date DESC",
            StudySort::PatientId => " ORDER BY s.patient_id COLLATE NOCASE, s.study_date DESC",
            StudySort::Modality => " ORDER BY s.modalities, s.study_date DESC",
        });

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), study_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
                        (SELECT COUNT(*) FROM image i JOIN series se ON i.series_id = se.id WHERE se.study_id = s.id)
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

    pub fn mark_study_opened(&self, id: i64) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE study SET date_opened = ?1 WHERE id = ?2",
            params![now_unix(), id],
        )?;
        Ok(())
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
            rows.filter_map(|r| r.ok())
                .map(|p| self.paths.resolve(&p))
                .collect()
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
                    se.comment, se.date_added
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
                        se.comment, se.date_added
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

    // ------------------------------------------------------------------- rois

    /// ROIs stored for one frame, oldest first.
    pub fn rois(&self, image_id: i64, frame: i64) -> Result<Vec<RoiRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, image_id, frame, kind, points, name, value, unit FROM roi
             WHERE image_id = ?1 AND frame = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![image_id, frame], roi_from_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replace every ROI of one frame with `rois`.
    pub fn replace_rois(&self, image_id: i64, frame: i64, rois: &[RoiRecord]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM roi WHERE image_id = ?1 AND frame = ?2", params![image_id, frame])?;
        for r in rois {
            tx.execute(
                "INSERT INTO roi (image_id, frame, kind, points, name, value, unit, date_created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    image_id,
                    frame,
                    r.kind,
                    serde_json::to_string(&r.points).unwrap_or_else(|_| "[]".into()),
                    r.name,
                    r.value,
                    r.unit,
                    now_unix()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every named measurement in a study, for reports: (series id, ROI).
    pub fn named_measurements_for_study(&self, study_id: i64) -> Result<Vec<(i64, RoiRecord)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.image_id, r.frame, r.kind, r.points, r.name, r.value, r.unit, i.series_id
             FROM roi r JOIN image i ON i.id = r.image_id JOIN series s ON s.id = i.series_id
             WHERE s.study_id = ?1 AND r.name IS NOT NULL ORDER BY r.id",
        )?;
        let rows = stmt.query_map(params![study_id], |row| Ok((row.get::<_, i64>(8)?, roi_from_row(row)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM image WHERE path = ?1", params![path], |r| r.get(0))?;
        Ok(n > 0)
    }

    // ------------------------------------------------------------ calibration

    /// User calibration (row mm, column mm) for an image, overriding the header.
    pub fn calibration(&self, image_id: i64) -> Result<Option<[f64; 2]>> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT row_mm, col_mm FROM calibration WHERE image_id = ?1",
                params![image_id],
                |r| Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?]),
            )
            .optional()?)
    }

    pub fn set_calibration(&self, image_id: i64, row_mm: f64, col_mm: f64, description: &str) -> Result<()> {
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO calibration (image_id, row_mm, col_mm, description, date_set)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![image_id, row_mm, col_mm, description, now_unix()],
        )?;
        Ok(())
    }

    /// Apply one calibration to every image of a series.
    pub fn set_series_calibration(&self, series_id: i64, row_mm: f64, col_mm: f64, description: &str) -> Result<usize> {
        let images = self.images_for_series(series_id)?;
        for img in &images {
            self.set_calibration(img.id, row_mm, col_mm, description)?;
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
         CREATE INDEX IF NOT EXISTS roi_image ON roi(image_id, frame);",
    )?;
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
        let a = db.insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10).unwrap();
        assert!(matches!(a, InsertOutcome::Inserted { new_study: true, .. }));
        let b = db.insert_instance(&meta("1", "1.1", "1.1.2"), "b.dcm", true, 10).unwrap();
        assert!(matches!(b, InsertOutcome::Inserted { new_study: false, .. }));
        let c = db.insert_instance(&meta("1", "1.1", "1.1.2"), "b.dcm", true, 10).unwrap();
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

        let f = StudyFilter { modality: "DX".into(), ..Default::default() };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);
        let f = StudyFilter { modality: "MR".into(), ..Default::default() };
        assert_eq!(db.list_studies(&f).unwrap().len(), 0);
        let f = StudyFilter { text: "fido".into(), ..Default::default() };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);
    }

    #[test]
    fn albums_and_delete() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { study_id, .. } =
            db.insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10).unwrap()
        else {
            panic!()
        };
        let album = db.create_album("Hips", None).unwrap();
        db.add_study_to_album(album, study_id).unwrap();
        let albums = db.albums().unwrap();
        assert_eq!(albums[0].number_of_studies, 1);
        let f = StudyFilter { album_id: Some(album), ..Default::default() };
        assert_eq!(db.list_studies(&f).unwrap().len(), 1);

        let files = db.delete_study(study_id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(db.counts().unwrap(), (0, 0, 0));
        assert_eq!(db.albums().unwrap()[0].number_of_studies, 0);
    }

    #[test]
    fn rois_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { image_id, .. } =
            db.insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10).unwrap()
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
        };
        db.replace_rois(image_id, 0, &[rec.clone()]).unwrap();
        let got = db.rois(image_id, 0).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].points, rec.points);
        assert_eq!(got[0].name.as_deref(), Some("LVIDd"));
        db.replace_rois(image_id, 0, &[]).unwrap();
        assert!(db.rois(image_id, 0).unwrap().is_empty());
    }

    #[test]
    fn calibration_round_trip() {
        let db = Database::open_in_memory(&DataPaths::new("x")).unwrap();
        let InsertOutcome::Inserted { series_id, image_id, .. } =
            db.insert_instance(&meta("1", "1.1", "1.1.1"), "a.dcm", true, 10).unwrap()
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
