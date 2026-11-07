//! ruthos-net: DICOM networking on the dicom-rs upper layer.
//!
//! A C-STORE listener (modalities and C-MOVE retrievals send straight into
//! the archive), and Ruthos as a client of a PACS: echo, query (C-FIND),
//! retrieve (C-GET or C-MOVE) and send (C-STORE). Failures are classified
//! in `error` so the interface can say what went wrong in plain words.

pub mod dicomweb;
pub mod dimse;
pub mod error;
pub mod scp;
pub mod scu;
pub mod workflow;

pub use error::{explain, local_address, Me, NetError, Node, Problem};
pub use scp::{ListenerConfig, NetEvent, StoreScp};
pub use scu::{
    echo, find_series, find_studies, retrieve_series, retrieve_study, send_files, RetrieveMethod, RetrieveOutcome,
    RetrieveProgress, SendOutcome, SeriesResult, StudyQuery, StudyResult,
};
pub use workflow::{find_worklist, request_commitment, CommitOutcome, Commitment, WorkItem, WorklistQuery};
