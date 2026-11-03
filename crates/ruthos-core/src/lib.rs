//! ruthos-core: the patient archive.
//!
//! Owns the on-disk layout, the SQLite database of studies, series and
//! images, the import pipeline, thumbnails, albums and application settings.
//! Nothing in here knows about windows or pixels on screen.

pub mod db;
pub mod import;
pub mod measure;
pub mod mmode;
pub mod incoming;
pub mod model;
pub mod paths;
pub mod report;
pub mod settings;
pub mod teaching;
pub mod thumbs;

pub use db::{Database, StudyFilter};
pub use import::{ImportEvent, ImportMode, Importer};
pub use model::{AlbumRow, DateFilter, ImageRow, MeasurementInput, MeasurementRow, ReportRow, RoiRecord, RoiValue, SeriesRow, StudyRow};
pub use paths::DataPaths;
pub use settings::{MeasurementSet, Settings};
pub use teaching::TeachingCase;
pub use thumbs::{ThumbnailService, ThumbnailUpdate};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Dicom(#[from] ruthos_dicom::DicomError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
