//! The list of models, with each one's terms.
#![cfg(feature = "ai")]

use ruthos_segment::ai::{Engine, AVAILABLE, NN_INTERACTIVE, SAM2_TINY, VISTA3D};

#[test]
fn sam_comes_first_and_needs_no_agreement() {
    assert_eq!(AVAILABLE[0].id, SAM2_TINY.id);
    assert_eq!(AVAILABLE[0].engine, Engine::Slices);
    assert!(AVAILABLE[0].research.is_none());
    assert!(AVAILABLE[0].downloadable());
}

#[test]
fn a_research_model_says_it_is_not_for_paid_work() {
    let terms = NN_INTERACTIVE.research.expect("a research model has terms");
    assert!(terms.contains("non-commercial"), "{terms}");
    assert!(terms.to_lowercase().contains("paid"), "{terms}");
    assert!(!NN_INTERACTIVE.downloadable(), "nothing is hosted for it");
    assert!(VISTA3D.research.is_none(), "its licence allows commercial use");
}

#[test]
fn each_model_has_its_own_folder() {
    let root = std::path::Path::new("root");
    let mut folders: Vec<_> = AVAILABLE.iter().map(|m| m.folder(root)).collect();
    folders.sort();
    folders.dedup();
    assert_eq!(folders.len(), AVAILABLE.len());
}

#[test]
fn only_research_mode_offers_research_models() {
    let open = |research| {
        AVAILABLE
            .iter()
            .filter(|m| m.offered(research))
            .map(|m| m.id)
            .collect::<Vec<_>>()
    };
    assert!(!open(false).contains(&NN_INTERACTIVE.id));
    assert!(open(true).contains(&NN_INTERACTIVE.id));
    assert!(open(false).contains(&VISTA3D.id));
}
