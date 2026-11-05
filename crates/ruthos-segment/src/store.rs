//! Segments to and from DICOM Segmentation objects.
//!
//! A SEG says how each segment was made in the standard's terms. This says what
//! Ruthos's own records mean in those terms, and reads them back, so that a saved
//! segment returns with the same mark, and one from elsewhere keeps what it said.

use std::collections::BTreeMap;

use ruthos_dicom::seg::{Algorithm, Code, Placed, Segment as SegSegment, SegmentData, Term};

use crate::{Grid, Mask, Provenance, Segment, Segmentation};

/// Marks a Ruthos tool's algorithm name.
const OURS: &str = "Ruthos ";
/// Models whose names mark a segment as theirs, with the version each records.
const MODELS: [(&str, &str); 3] = [
    ("SAM 2.1", "2.1"),
    ("VISTA3D", "NV-Segment-CT"),
    ("nnInteractive", "1.0"),
];
const REFINED: &str = " with Ruthos edge refinement";
const EDITED: &str = ", edited";

/// Used when a segment was never given a term: the broadest in the list.
const DEFAULT_TERM: &str = "Other tissue";

/// A model's result is never AUTOMATIC: a person's clicks chose the object.
pub fn algorithm(p: &Provenance) -> Algorithm {
    match p {
        Provenance::Manual => Algorithm::Manual,
        Provenance::SemiAutomatic { tool } => Algorithm::SemiAutomatic(format!("{OURS}{}", tool.to_lowercase())),
        Provenance::Automatic {
            model, refined, edited, ..
        } => {
            let refined = if *refined { REFINED } else { "" };
            let edited = if *edited { EDITED } else { "" };
            Algorithm::SemiAutomatic(format!("{model}{refined}{edited}"))
        }
    }
}

/// How a segment read from a SEG was made, in Ruthos's terms. A name from
/// elsewhere is kept as it is.
pub fn provenance(a: &Algorithm) -> Provenance {
    let tool = |name: &str| {
        let mut letters = name.chars();
        let first = letters.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        first + letters.as_str()
    };
    match a {
        Algorithm::Manual => Provenance::Manual,
        Algorithm::SemiAutomatic(name) if MODELS.iter().any(|(m, _)| name.starts_with(m)) => {
            let version = MODELS.iter().find(|(m, _)| name.starts_with(m)).map_or("", |m| m.1);
            let (name, edited) = match name.strip_suffix(EDITED) {
                Some(n) => (n, true),
                None => (name.as_str(), false),
            };
            let (model, refined) = match name.strip_suffix(REFINED) {
                Some(n) => (n, true),
                None => (name, false),
            };
            Provenance::Automatic {
                model: model.to_string(),
                version: version.to_string(),
                refined,
                edited,
            }
        }
        Algorithm::SemiAutomatic(name) => Provenance::SemiAutomatic {
            tool: name.strip_prefix(OURS).map(tool).unwrap_or_else(|| name.clone()),
        },
        Algorithm::Automatic(name) => Provenance::Automatic {
            model: name.clone(),
            version: String::new(),
            refined: false,
            edited: false,
        },
    }
}

fn coding(s: &Segment) -> (Code, Code) {
    s.coding.clone().unwrap_or_else(|| {
        let t = Term::by_label(DEFAULT_TERM).unwrap_or(&ruthos_dicom::seg::TERMS[0]);
        (t.category(), t.kind())
    })
}

/// Every segment's voxels, as masks per slice.
fn slices_of(seg: &Segmentation) -> BTreeMap<u8, BTreeMap<usize, Vec<bool>>> {
    let [w, h, _] = seg.grid.dims;
    let plane = w * h;
    let mut out: BTreeMap<u8, BTreeMap<usize, Vec<bool>>> = BTreeMap::new();
    for (z, slice) in seg.labels.chunks_exact(plane).enumerate() {
        for (i, label) in slice.iter().enumerate().filter(|(_, l)| **l != 0) {
            out.entry(*label)
                .or_default()
                .entry(z)
                .or_insert_with(|| vec![false; plane])[i] = true;
        }
    }
    out
}

/// A model's own mask to save beside a segment: the segment's label, and for each
/// slice the voxels in it, sorted.
pub struct Unrefined<'a> {
    pub label: u8,
    pub slices: &'a BTreeMap<usize, Vec<u32>>,
}

/// The segments of `seg`, numbered as they are labelled, ready to write. With
/// `unrefined`, each model's own mask follows as a segment of its own, for scoring
/// a case with and without refinement.
pub fn to_seg(seg: &Segmentation, unrefined: &[Unrefined<'_>]) -> Vec<SegmentData> {
    let mut slices = slices_of(seg);
    let describe = |s: &Segment, number: u16, label: String, algorithm: Algorithm| {
        let (category, kind) = coding(s);
        SegSegment {
            number,
            label,
            colour: s.colour,
            algorithm,
            category,
            kind,
        }
    };
    let mut out: Vec<SegmentData> = seg
        .segments
        .iter()
        .filter_map(|s| {
            let voxels = slices.remove(&s.label)?;
            Some(SegmentData {
                segment: describe(s, s.label as u16, s.name.clone(), algorithm(&s.provenance)),
                slices: voxels,
            })
        })
        .collect();
    let plane = seg.grid.dims[0] * seg.grid.dims[1];
    let mut number = 255;
    for raw in unrefined {
        let Some(s) = seg.segments.iter().find(|s| s.label == raw.label) else {
            continue;
        };
        let Provenance::Automatic { model, .. } = &s.provenance else {
            continue;
        };
        number += 1;
        let voxels = raw
            .slices
            .iter()
            .map(|(z, on)| {
                let mut mask = vec![false; plane];
                on.iter().for_each(|i| mask[*i as usize] = true);
                (*z, mask)
            })
            .collect();
        out.push(SegmentData {
            segment: describe(
                s,
                number,
                format!("{} (unrefined)", s.name),
                Algorithm::SemiAutomatic(model.clone()),
            ),
            slices: voxels,
        });
    }
    out
}

/// Segments read from a SEG as a label map, and how many voxels two segments both
/// claimed, which a label map cannot hold: the later segment keeps those. A SEG
/// with more segments than there are labels loses the last, which is counted too.
pub fn from_placed(grid: Grid, placed: Vec<Placed>) -> (Segmentation, usize, usize) {
    let mut seg = Segmentation::new(grid);
    let (mut overlapping, mut left_out) = (0, 0);
    for p in placed {
        if p.voxels.is_empty() {
            continue;
        }
        let mut mask = Mask::empty(grid);
        for &i in &p.voxels {
            if i < grid.len() {
                overlapping += usize::from(seg.labels[i] != 0);
                mask.on[i] = true;
            }
        }
        let provenance = provenance(&p.segment.algorithm);
        let colour = p.segment.colour;
        match seg.add(&p.segment.label, colour, provenance, &mask) {
            Some(label) => {
                if let Some(s) = seg.segments.iter_mut().find(|s| s.label == label) {
                    s.coding = Some((p.segment.category, p.segment.kind));
                }
            }
            None => left_out += 1,
        }
    }
    (seg, overlapping, left_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            dims: [4, 3, 3],
            spacing: [1.0, 1.0, 2.0],
        }
    }

    #[test]
    fn each_provenance_survives_the_trip() {
        for p in [
            Provenance::Manual,
            Provenance::SemiAutomatic {
                tool: "Region grow".to_string(),
            },
            Provenance::Automatic {
                model: "SAM 2.1 Tiny".to_string(),
                version: "2.1".to_string(),
                refined: true,
                edited: false,
            },
            Provenance::Automatic {
                model: "SAM 2.1 Tiny".to_string(),
                version: "2.1".to_string(),
                refined: true,
                edited: true,
            },
            Provenance::Automatic {
                model: "SAM 2.1 Tiny".to_string(),
                version: "2.1".to_string(),
                refined: false,
                edited: false,
            },
        ] {
            assert_eq!(provenance(&algorithm(&p)), p);
        }
    }

    #[test]
    fn the_names_are_those_the_standard_will_show() {
        let refined = Provenance::Automatic {
            model: "SAM 2.1 Tiny".to_string(),
            version: "2.1".to_string(),
            refined: true,
            edited: false,
        };
        assert_eq!(
            algorithm(&refined),
            Algorithm::SemiAutomatic("SAM 2.1 Tiny with Ruthos edge refinement".to_string())
        );
        let grown = Provenance::SemiAutomatic {
            tool: "Region grow".to_string(),
        };
        assert_eq!(
            algorithm(&grown),
            Algorithm::SemiAutomatic("Ruthos region grow".to_string())
        );
        assert_eq!(algorithm(&Provenance::Manual), Algorithm::Manual);
    }

    #[test]
    fn a_name_from_elsewhere_is_kept() {
        let foreign = Algorithm::SemiAutomatic("Slicer Grow From Seeds".to_string());
        assert_eq!(
            provenance(&foreign),
            Provenance::SemiAutomatic {
                tool: "Slicer Grow From Seeds".to_string()
            }
        );
        let pacs = Algorithm::Automatic("Vendor AI 4".to_string());
        assert!(matches!(provenance(&pacs), Provenance::Automatic { model, .. } if model == "Vendor AI 4"));
    }

    #[test]
    fn only_slices_with_a_segment_are_given() {
        let g = grid();
        let mut seg = Segmentation::new(g);
        let mut m = Mask::empty(g);
        m.on[g.index(1, 1, 2)] = true;
        seg.add("A", [1, 2, 3], Provenance::Manual, &m).unwrap();
        seg.add("Empty", [4, 5, 6], Provenance::Manual, &Mask::empty(g))
            .unwrap();
        let out = to_seg(&seg, &[]);
        assert_eq!(out.len(), 1, "an empty segment is not written");
        assert_eq!(out[0].slices.keys().copied().collect::<Vec<_>>(), [2]);
        assert!(out[0].slices[&2][1 + 4]);
    }

    #[test]
    fn an_unrefined_mask_goes_as_a_segment_of_its_own() {
        let g = grid();
        let mut seg = Segmentation::new(g);
        let mut m = Mask::empty(g);
        m.on[0] = true;
        let label = seg
            .add(
                "Bone",
                [1, 2, 3],
                Provenance::Automatic {
                    model: "SAM 2.1 Tiny".to_string(),
                    version: "2.1".to_string(),
                    refined: true,
                    edited: false,
                },
                &m,
            )
            .unwrap();
        let mut raw = BTreeMap::new();
        raw.insert(0usize, vec![0u32, 1]);
        let out = to_seg(&seg, &[Unrefined { label, slices: &raw }]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].segment.label, "Bone (unrefined)");
        assert_eq!(
            out[1].segment.algorithm,
            Algorithm::SemiAutomatic("SAM 2.1 Tiny".to_string())
        );
        assert!(out[1].segment.number > 255, "clear of every label");
        assert_eq!(out[1].slices[&0].iter().filter(|v| **v).count(), 2);
    }

    #[test]
    fn overlaps_are_counted_and_an_unknown_code_is_kept() {
        let g = grid();
        let unknown = Code {
            value: "99999".to_string(),
            scheme: "99TEST".to_string(),
            meaning: "Not known".to_string(),
        };
        let segment = |number: u16, voxels: Vec<usize>| Placed {
            segment: SegSegment {
                number,
                label: format!("S{number}"),
                colour: [10, 20, 30],
                algorithm: Algorithm::Manual,
                category: unknown.clone(),
                kind: unknown.clone(),
            },
            voxels,
        };
        let (seg, overlapping, left_out) = from_placed(
            g,
            vec![segment(1, vec![0, 1]), segment(2, vec![1, 2]), segment(3, Vec::new())],
        );
        assert_eq!((overlapping, left_out), (1, 0));
        assert_eq!(seg.segments.len(), 2, "an empty one is dropped");
        assert_eq!(seg.segments[1].coding.as_ref().unwrap().1, unknown);
        assert_eq!(seg.segments[0].colour, [10, 20, 30]);
    }
}
