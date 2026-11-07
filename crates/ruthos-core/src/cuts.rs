//! What was cut away in a series's 3D view, kept with the study so it is there
//! the next time.
//!
//! The mask holds, for each voxel, 0 or the number of the cut that took it, so
//! Undo still removes the last cut after reopening. It is mostly zeros, so it is
//! stored deflated.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// The live clip box, in voxel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedClip {
    pub low: [f64; 3],
    pub high: [f64; 3],
    /// Which standard cut it began as, if one.
    pub part: Option<usize>,
    /// It shows everything except what it holds.
    pub removes: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedCuts {
    /// The volume the mask belongs to: a mask is only used on the same grid.
    pub dims: [usize; 3],
    pub mask: Vec<u8>,
    /// How many cuts the mask holds.
    pub count: u8,
    pub clip: Option<SavedClip>,
}

impl SavedCuts {
    /// Nothing cut and no clip: nothing worth keeping.
    pub fn is_empty(&self) -> bool {
        self.count == 0 && self.clip.is_none()
    }
}

/// The mask, deflated.
pub fn pack(mask: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    // Writing to memory does not fail.
    let _ = encoder.write_all(mask);
    encoder.finish().unwrap_or_default()
}

/// The mask from its deflated bytes, if it is exactly `voxels` long.
pub fn unpack(packed: &[u8], voxels: usize) -> Option<Vec<u8>> {
    let mut mask = Vec::with_capacity(voxels);
    // One more than expected is enough to tell a mask that is too long.
    flate2::read::DeflateDecoder::new(packed)
        .take(voxels as u64 + 1)
        .read_to_end(&mut mask)
        .ok()?;
    (mask.len() == voxels).then_some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_packs_small_and_comes_back() {
        let mut mask = vec![0u8; 64 * 64 * 64];
        mask[1000..5000].fill(1);
        mask[90_000..91_000].fill(2);
        let packed = pack(&mask);
        assert!(packed.len() < mask.len() / 50, "{} bytes", packed.len());
        assert_eq!(unpack(&packed, mask.len()).unwrap(), mask);
        assert!(unpack(&packed, mask.len() - 1).is_none(), "the wrong size is refused");
        assert!(unpack(&packed, mask.len() + 1).is_none());
        assert!(unpack(b"not deflate", 10).is_none());
    }
}
