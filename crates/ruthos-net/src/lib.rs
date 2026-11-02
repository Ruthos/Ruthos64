//! ruthos-net: DICOM networking on the dicom-rs upper layer.
//!
//! Tier 1 ships a C-STORE listener (so modalities can send straight into the
//! archive) and a C-ECHO client for testing PACS nodes. Query/retrieve and
//! send follow the same patterns and land here next.

pub mod echo;
pub mod scp;

pub use echo::echo;
pub use scp::{ListenerConfig, NetEvent, StoreScp};
