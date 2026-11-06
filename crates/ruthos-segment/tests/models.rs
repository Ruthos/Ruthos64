//! The list of models, with each one's terms.
#![cfg(feature = "ai")]

use ruthos_segment::ai::{Engine, Item, Manifest, AVAILABLE, NN_INTERACTIVE, SAM2_TINY, VISTA3D};

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
}

#[test]
fn vista3d_is_research_only_per_its_model_card() {
    let terms = VISTA3D.research.expect("NVIDIA's model card restricts it to research");
    assert!(terms.contains("research"), "{terms}");
    assert!(terms.to_lowercase().contains("non-clinical"), "{terms}");
    assert!(terms.contains("NVIDIA Open Model License"), "{terms}");
}

/// Licences under which a model may be offered to everyone. A model under any
/// other licence, or whose source restricts it to research, must carry
/// research terms, so that giving it a download address can never make it
/// generally available.
#[test]
fn every_restricted_model_has_research_terms() {
    const GENERAL_USE: &[&str] = &["Apache 2.0", "MIT", "BSD-3-Clause"];
    for m in &AVAILABLE {
        let restricted = !GENERAL_USE.contains(&m.licence)
            || m.training.to_lowercase().contains("research")
            || m.licence.to_lowercase().contains("non-commercial");
        if restricted {
            assert!(
                m.research.is_some(),
                "{} ({}) is restricted but has no research terms",
                m.name,
                m.licence
            );
        }
    }
}

/// A download address alone must not put a research model in front of a user
/// who has research mode off.
#[test]
fn a_download_address_does_not_make_a_research_model_general() {
    const HOSTED: &[Item] = &[Item {
        name: "vista3d-encoder-fp16.onnx",
        url: "https://example.invalid/vista3d-encoder-fp16.onnx",
        bytes: 1,
        sha256: "",
        entry: None,
    }];
    let hosted = Manifest {
        model: HOSTED,
        ..VISTA3D
    };
    assert!(hosted.downloadable());
    assert!(!hosted.offered(false), "research mode off: not offered");
    assert!(hosted.offered(true));
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
    assert!(open(false).contains(&SAM2_TINY.id));
    assert!(!open(false).contains(&VISTA3D.id), "research only");
    assert!(open(true).contains(&VISTA3D.id));
}
