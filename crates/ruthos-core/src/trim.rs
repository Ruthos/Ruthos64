//! Trimming an echo loop to the frames worth playing, often one cardiac
//! cycle.
//!
//! The trim is kept in the database and never changes the file: the loop
//! plays, and exports as a movie, between its first and last frame. A
//! trimmed copy can also be written as a new instance of the same series,
//! keeping the original's compression (its frames are copied, not
//! re-encoded) and marked as derived from it.

use std::path::Path;

use dicom_core::value::{PrimitiveValue, Value};
use dicom_core::{dicom_value, DataElement, VR};
use dicom_dictionary_std::tags;

use ruthos_dicom::open::open_any;

use crate::mpr_export::new_uid;
use crate::{CoreError, Result};

/// The frames of a loop that play: `first` to `last`, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopTrim {
    pub first: u32,
    pub last: u32,
}

impl LoopTrim {
    /// A trim of a loop of `frames`, put in order and kept inside it; `None`
    /// when it would keep every frame (nothing is trimmed) or fewer than two.
    pub fn new(first: u32, last: u32, frames: u32) -> Option<Self> {
        if frames < 2 {
            return None;
        }
        let (a, b) = (first.min(last), first.max(last).min(frames - 1));
        (b > a && !(a == 0 && b == frames - 1)).then_some(Self { first: a, last: b })
    }

    /// How many frames play.
    pub fn len(&self) -> usize {
        (self.last - self.first + 1) as usize
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn contains(&self, frame: u32) -> bool {
        (self.first..=self.last).contains(&frame)
    }

    /// The frame `by` after `frame`, going round inside the trim.
    pub fn after(&self, frame: u32, by: u32) -> u32 {
        let n = self.len() as u32;
        let at = frame.clamp(self.first, self.last) - self.first;
        self.first + (at + by) % n
    }
}

/// Write the frames of `trim` from the multi-frame file `source` to `out`
/// as a new instance of the same series. Returns its SOP Instance UID.
pub fn write_trimmed_copy(source: &Path, out: &Path, trim: LoopTrim) -> Result<String> {
    let mut obj = open_any(source, None)?;
    let int = |obj: &dicom_object::DefaultDicomObject, tag| {
        obj.element(tag).ok().and_then(|e| e.to_int::<i64>().ok()).unwrap_or(0)
    };
    let frames = int(&obj, tags::NUMBER_OF_FRAMES).max(1) as u32;
    if trim.last >= frames {
        return Err(CoreError::Other(format!(
            "the loop has {frames} frames, fewer than the trim needs"
        )));
    }
    if obj.element(tags::PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE).is_ok() {
        return Err(CoreError::Other(
            "an enhanced multi-frame image cannot be trimmed into a copy yet".into(),
        ));
    }
    let keep = trim.first as usize..=trim.last as usize;

    // The pixels: whole frames copied, in their own compression.
    let pixels = obj
        .take_element(tags::PIXEL_DATA)
        .map_err(|_| CoreError::Other("the image has no pixel data".into()))?;
    let (header, value) = (*pixels.header(), pixels.into_value());
    let value = match value {
        Value::PixelSequence(seq) => {
            let (table, fragments) = seq.into_parts();
            let per_frame = frame_fragments(&table, &fragments, frames as usize)?;
            let kept: Vec<Vec<u8>> = per_frame[keep.clone()]
                .iter()
                .flat_map(|range| fragments[range.clone()].iter().cloned())
                .collect();
            Value::PixelSequence(dicom_core::value::PixelFragmentSequence::new_fragments(kept))
        }
        Value::Primitive(p) => {
            let bytes = p.to_bytes();
            let rows = int(&obj, tags::ROWS) as usize;
            let cols = int(&obj, tags::COLUMNS) as usize;
            let samples = int(&obj, tags::SAMPLES_PER_PIXEL).max(1) as usize;
            let bits = int(&obj, tags::BITS_ALLOCATED).max(8) as usize;
            let photometric = obj
                .element(tags::PHOTOMETRIC_INTERPRETATION)
                .ok()
                .and_then(|e| e.to_str().ok().map(|s| s.trim().to_string()))
                .unwrap_or_default();
            // 4:2:2 holds two samples a pixel, whatever it says.
            let samples = if photometric.ends_with("_422") { 2 } else { samples };
            if !bits.is_multiple_of(8) {
                return Err(CoreError::Other("the image's pixel packing cannot be trimmed".into()));
            }
            let frame_len = rows * cols * samples * bits / 8;
            if frame_len == 0 || frame_len * frames as usize > bytes.len() {
                return Err(CoreError::Other("the pixel data is shorter than its frames".into()));
            }
            let mut kept = bytes[trim.first as usize * frame_len..(trim.last as usize + 1) * frame_len].to_vec();
            if kept.len() % 2 == 1 {
                kept.push(0);
            }
            Value::Primitive(PrimitiveValue::from(kept))
        }
        Value::Sequence(_) => return Err(CoreError::Other("the pixel data is not an image".into())),
    };
    obj.put(DataElement::new(header.tag, header.vr, value));

    // What describes the frames, cut to match.
    let s = |obj: &mut dicom_object::DefaultDicomObject, tag, vr, v: &str| {
        obj.put(DataElement::new(tag, vr, dicom_value!(Str, v)))
    };
    s(&mut obj, tags::NUMBER_OF_FRAMES, VR::IS, &trim.len().to_string());
    if let Some(times) = obj
        .element(tags::FRAME_TIME_VECTOR)
        .ok()
        .and_then(|e| e.to_multi_float64().ok())
        .filter(|t| t.len() == frames as usize)
    {
        // Each entry is the time since the frame before; the first is zero.
        let mut cut: Vec<f64> = vec![0.0];
        cut.extend(&times[trim.first as usize + 1..=trim.last as usize]);
        let text: Vec<String> = cut.iter().map(|t| format!("{t}")).collect();
        s(&mut obj, tags::FRAME_TIME_VECTOR, VR::DS, &text.join("\\"));
    } else {
        obj.remove_element(tags::FRAME_TIME_VECTOR);
    }
    obj.remove_element(tags::START_TRIM);
    obj.remove_element(tags::STOP_TRIM);

    // A new instance, saying where it came from.
    let original = obj
        .element(tags::SOP_INSTANCE_UID)
        .ok()
        .and_then(|e| e.to_str().ok().map(|s| s.trim_end_matches('\0').to_string()))
        .unwrap_or_default();
    let image_type = obj
        .element(tags::IMAGE_TYPE)
        .ok()
        .and_then(|e| e.to_str().ok().map(|s| s.to_string()))
        .unwrap_or_default();
    let mut values: Vec<&str> = image_type.split('\\').collect();
    match values.first_mut() {
        Some(v) if !v.trim().is_empty() => *v = "DERIVED",
        _ => values = vec!["DERIVED", "PRIMARY"],
    }
    if values.len() < 2 {
        values.push("PRIMARY");
    }
    s(&mut obj, tags::IMAGE_TYPE, VR::CS, &values.join("\\"));
    s(
        &mut obj,
        tags::DERIVATION_DESCRIPTION,
        VR::ST,
        &format!(
            "Frames {} to {} of {} of instance {original}, trimmed in Ruthos",
            trim.first + 1,
            trim.last + 1,
            frames
        ),
    );
    let sop = new_uid();
    s(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, &sop);
    let meta = obj.meta_mut();
    meta.media_storage_sop_instance_uid = sop.clone();
    meta.update_information_group_length();
    obj.write_to_file(out).map_err(|e| CoreError::Other(e.to_string()))?;
    Ok(sop)
}

/// Which fragments make each frame of encapsulated pixel data: one each,
/// or as the basic offset table divides them.
fn frame_fragments(table: &[u32], fragments: &[Vec<u8>], frames: usize) -> Result<Vec<std::ops::Range<usize>>> {
    if fragments.len() == frames {
        return Ok((0..frames).map(|i| i..i + 1).collect());
    }
    if table.len() != frames {
        return Err(CoreError::Other(
            "the loop's frames cannot be told apart in its compressed data".into(),
        ));
    }
    // Where each fragment starts, counted as the offset table counts: from
    // the first fragment's item tag, 8 bytes of item header each.
    let mut starts = Vec::with_capacity(fragments.len());
    let mut at = 0u64;
    for f in fragments {
        starts.push(at);
        at += 8 + f.len() as u64;
    }
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let from = table[i] as u64;
        let to = table.get(i + 1).map(|t| *t as u64).unwrap_or(u64::MAX);
        let a = starts.iter().position(|s| *s >= from).unwrap_or(fragments.len());
        let b = starts.iter().position(|s| *s >= to).unwrap_or(fragments.len());
        if b <= a {
            return Err(CoreError::Other(
                "the loop's offset table does not match its data".into(),
            ));
        }
        out.push(a..b);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trim_keeps_inside_the_loop_and_goes_round() {
        assert_eq!(LoopTrim::new(0, 9, 10), None, "the whole loop is no trim");
        assert_eq!(LoopTrim::new(4, 4, 10), None, "one frame is no loop");
        let t = LoopTrim::new(7, 3, 10).unwrap();
        assert_eq!((t.first, t.last, t.len()), (3, 7, 5));
        assert_eq!(LoopTrim::new(2, 40, 10).unwrap().last, 9);
        assert_eq!(t.after(3, 1), 4);
        assert_eq!(t.after(7, 1), 3, "round to the first");
        assert_eq!(t.after(6, 4), 5);
        assert_eq!(t.after(0, 0), 3, "a frame outside starts at the first");
    }

    #[test]
    fn a_trimmed_copy_keeps_its_frames_and_says_where_it_came_from() {
        use dicom_dictionary_std::uids;
        use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
        let dir = std::env::temp_dir().join(format!("ruthos-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("loop.dcm"), dir.join("cut.dcm"));
        // Five 2x2 frames, each filled with its own number.
        let mut obj = InMemDicomObject::new_empty();
        let s = |obj: &mut InMemDicomObject, tag, vr, v: &str| obj.put(DataElement::new(tag, vr, dicom_value!(Str, v)));
        s(
            &mut obj,
            tags::SOP_CLASS_UID,
            VR::UI,
            uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE,
        );
        s(&mut obj, tags::SOP_INSTANCE_UID, VR::UI, "1.2.3.4");
        s(&mut obj, tags::NUMBER_OF_FRAMES, VR::IS, "5");
        s(&mut obj, tags::FRAME_TIME_VECTOR, VR::DS, "0\\10\\20\\30\\40");
        s(&mut obj, tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
        for (tag, v) in [
            (tags::ROWS, 2u16),
            (tags::COLUMNS, 2),
            (tags::SAMPLES_PER_PIXEL, 1),
            (tags::BITS_ALLOCATED, 8),
            (tags::BITS_STORED, 8),
            (tags::HIGH_BIT, 7),
            (tags::PIXEL_REPRESENTATION, 0),
        ] {
            obj.put(DataElement::new(tag, VR::US, dicom_value!(U16, [v])));
        }
        let pixels: Vec<u8> = (0..5u8).flat_map(|f| [f; 4]).collect();
        obj.put(DataElement::new(tags::PIXEL_DATA, VR::OB, PrimitiveValue::from(pixels)));
        let meta = FileMetaTableBuilder::new()
            .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
            .media_storage_sop_class_uid(uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE)
            .media_storage_sop_instance_uid("1.2.3.4")
            .build()
            .unwrap();
        obj.with_exact_meta(meta).write_to_file(&src).unwrap();

        let sop = write_trimmed_copy(&src, &out, LoopTrim::new(1, 3, 5).unwrap()).unwrap();
        let cut = open_any(&out, None).unwrap();
        let text = |tag| cut.element(tag).unwrap().to_str().unwrap().trim().to_string();
        assert_eq!(text(tags::NUMBER_OF_FRAMES), "3");
        assert_eq!(text(tags::SOP_INSTANCE_UID).trim_end_matches('\0'), sop);
        assert_eq!(cut.meta().media_storage_sop_instance_uid.trim_end_matches('\0'), sop);
        assert!(text(tags::IMAGE_TYPE).starts_with("DERIVED"));
        assert!(text(tags::DERIVATION_DESCRIPTION).contains("1.2.3.4"));
        assert_eq!(
            cut.element(tags::FRAME_TIME_VECTOR)
                .unwrap()
                .to_multi_float64()
                .unwrap(),
            vec![0.0, 20.0, 30.0]
        );
        let bytes = cut.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap().to_vec();
        assert_eq!(bytes, vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fragments_are_shared_out_by_the_offset_table() {
        let f = vec![vec![0u8; 10], vec![0u8; 4], vec![0u8; 6], vec![0u8; 2]];
        // Frame 0: fragments 0 and 1 (18 + 12 bytes); frame 1: 2 and 3.
        let r = frame_fragments(&[0, 30], &f, 2).unwrap();
        assert_eq!(r, vec![0..2, 2..4]);
        assert_eq!(frame_fragments(&[], &f, 4).unwrap()[3], 3..4);
        assert!(frame_fragments(&[], &f, 2).is_err());
    }
}
