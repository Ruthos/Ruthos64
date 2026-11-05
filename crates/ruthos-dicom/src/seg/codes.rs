//! The short list of SNOMED CT terms offered for a segment. A free-text label is
//! always kept beside the code, and a code Ruthos does not know is carried as it
//! came, so a SEG from elsewhere keeps what it said.

use serde::{Deserialize, Serialize};

/// A coded concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code {
    pub value: String,
    pub scheme: String,
    pub meaning: String,
}

impl Code {
    pub fn snomed(value: &str, meaning: &str) -> Self {
        Self {
            value: value.to_string(),
            scheme: "SCT".to_string(),
            meaning: meaning.to_string(),
        }
    }
}

/// One entry in the panel's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    pub label: &'static str,
    category: (&'static str, &'static str),
    kind: (&'static str, &'static str),
}

const TISSUE: (&str, &str) = ("85756007", "Tissue");
const ANATOMY: (&str, &str) = ("91723000", "Anatomical Structure");
const ABNORMAL: (&str, &str) = ("49755003", "Morphologically Abnormal Structure");
const SUBSTANCE: (&str, &str) = ("105590001", "Substance");

/// The panel's list. Categories are those of DICOM CID 7150.
pub const TERMS: [Term; 6] = [
    Term {
        label: "Bone",
        category: TISSUE,
        kind: ("272673000", "Bone"),
    },
    Term {
        label: "Tooth",
        category: ANATOMY,
        kind: ("38199008", "Tooth"),
    },
    Term {
        label: "Soft tissue",
        category: TISSUE,
        kind: ("87784001", "Soft tissue"),
    },
    Term {
        label: "Mass",
        category: ABNORMAL,
        kind: ("4147007", "Mass"),
    },
    Term {
        label: "Fluid",
        category: SUBSTANCE,
        kind: ("32457005", "Body fluid"),
    },
    Term {
        label: "Other tissue",
        category: TISSUE,
        kind: TISSUE,
    },
];

impl Term {
    pub fn category(&self) -> Code {
        Code::snomed(self.category.0, self.category.1)
    }

    pub fn kind(&self) -> Code {
        Code::snomed(self.kind.0, self.kind.1)
    }

    pub fn by_label(label: &str) -> Option<&'static Term> {
        TERMS.iter().find(|t| t.label == label)
    }

    /// The entry a code is, if it is one of ours.
    pub fn of(kind: &Code) -> Option<&'static Term> {
        TERMS.iter().find(|t| t.kind.0 == kind.value && kind.scheme == "SCT")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_term_is_found_by_its_code_and_by_its_label() {
        for t in &TERMS {
            assert_eq!(Term::of(&t.kind()), Some(t));
            assert_eq!(Term::by_label(t.label), Some(t));
            assert_eq!(t.kind().scheme, "SCT");
        }
        assert_eq!(Term::of(&Code::snomed("1", "Unknown")), None);
    }
}
