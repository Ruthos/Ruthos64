#!/usr/bin/env python3
"""Have other tools check Ruthos's DICOM SEG, by hand, with highdicom installed.

    RUTHOS_SEG_INTEROP=<empty folder> cargo test -p ruthos-dicom --test seg interoperates
    python scripts/seg_interop.py <folder>
    RUTHOS_SEG_INTEROP=<folder> cargo test -p ruthos-dicom --test seg interoperates -- --nocapture

The first step writes a CT series and ruthos-seg.dcm. This script reads that with
highdicom and pydicom and compares every voxel, then has highdicom write a SEG of
its own for the same CT. The last step reads that in Ruthos and compares again.
"""
import sys
from pathlib import Path

import highdicom as hd
import numpy as np
import pydicom
from pydicom.sr.codedict import codes


def check_ruthos(folder):
    """Ruthos's SEG, as highdicom reads it."""
    seg = hd.seg.segread(folder / "ruthos-seg.dcm")
    want = {}
    for line in (folder / "ruthos-voxels.txt").read_text().splitlines():
        label, voxels = line.split(":", 1)
        want[label] = sorted(int(v) for v in voxels.split())
    ds = pydicom.dcmread(folder / "ruthos-seg.dcm")
    assert ds.Modality == "SEG" and ds.SegmentationType == "BINARY"
    assert ds.BitsAllocated == 1 and ds.BitsStored == 1 and ds.HighBit == 0
    slices = sorted(
        (pydicom.dcmread(f, stop_before_pixels=True) for f in (folder / "ct").iterdir()),
        key=lambda d: float(d.ImagePositionPatient[2]),
    )
    uids = [d.SOPInstanceUID for d in slices]
    rows, columns = int(ds.Rows), int(ds.Columns)
    pixels = ds.pixel_array if int(ds.NumberOfFrames) > 1 else ds.pixel_array[None]
    for number in seg.segment_numbers:
        desc = seg.get_segment_description(number)
        label = desc.segment_label
        frames = [
            i
            for i, f in enumerate(ds.PerFrameFunctionalGroupsSequence)
            if f.SegmentIdentificationSequence[0].ReferencedSegmentNumber == number
        ]
        got = []
        for i in frames:
            groups = ds.PerFrameFunctionalGroupsSequence[i]
            source = groups.DerivationImageSequence[0].SourceImageSequence[0]
            z = uids.index(source.ReferencedSOPInstanceUID)
            on = np.flatnonzero(pixels[i].ravel())
            got.extend(int(p) + rows * columns * z for p in on)
        assert sorted(got) == want[label], label
        kind = desc.segmented_property_type
        print(
            f"highdicom: {label}: {len(got)} voxels match; {desc.algorithm_type.value}, "
            f"{kind.scheme_designator} {kind.value} {kind.meaning}"
        )
    volume = seg.get_volume(combine_segments=True, skip_overlap_checks=True)
    print("highdicom built a volume of", volume.array.shape)
    same = ds.ReferencedSeriesSequence[0].SeriesInstanceUID == slices[0].SeriesInstanceUID
    print("refers to the source series:", same)
    assert same


def write_highdicom(folder):
    """A SEG of highdicom's own for the same CT, and the voxels it holds."""
    sources = sorted(
        (pydicom.dcmread(f) for f in (folder / "ct").iterdir()),
        key=lambda d: float(d.ImagePositionPatient[2]),
    )
    rows, columns = int(sources[0].Rows), int(sources[0].Columns)
    mask = np.zeros((len(sources), rows, columns), dtype=np.uint8)
    rng = np.random.default_rng(5)
    mask[2:6, 5:20, 8:25] = 1
    mask[8:10, 20:28, 10:30] = 2
    speckle = mask == 1
    mask[speckle] = rng.integers(0, 2, speckle.sum())  # not just solid blocks
    algorithm = hd.AlgorithmIdentificationSequence(
        name="highdicom test", family=codes.cid7162.ArtificialIntelligence, version="1"
    )
    descriptions = [
        hd.seg.SegmentDescription(
            segment_number=1,
            segment_label="Foreign bone",
            segmented_property_category=codes.SCT.Tissue,
            segmented_property_type=codes.SCT.Bone,
            algorithm_type=hd.seg.SegmentAlgorithmTypeValues.AUTOMATIC,
            algorithm_identification=algorithm,
        ),
        hd.seg.SegmentDescription(
            segment_number=2,
            segment_label="Unknown thing",
            segmented_property_category=codes.SCT.Tissue,
            segmented_property_type=hd.sr.CodedConcept("99999", "99TEST", "Not a known code"),
            algorithm_type=hd.seg.SegmentAlgorithmTypeValues.MANUAL,
        ),
    ]
    seg = hd.seg.Segmentation(
        source_images=sources,
        pixel_array=mask,
        segmentation_type=hd.seg.SegmentationTypeValues.BINARY,
        segment_descriptions=descriptions,
        series_instance_uid=hd.UID(),
        series_number=901,
        sop_instance_uid=hd.UID(),
        instance_number=1,
        manufacturer="highdicom",
        manufacturer_model_name="test",
        software_versions="1",
        device_serial_number="1",
        omit_empty_frames=True,
    )
    seg.save_as(folder / "highdicom-seg.dcm")
    lines = []
    for number, label in ((1, "Foreign bone"), (2, "Unknown thing")):
        on = np.flatnonzero((mask == number).ravel())
        lines.append(f"{label}:" + " ".join(str(int(v)) for v in on))
    (folder / "highdicom-voxels.txt").write_text("\n".join(lines) + "\n")
    print(f"highdicom wrote a SEG of {int(seg.NumberOfFrames)} frames")


if __name__ == "__main__":
    root = Path(sys.argv[1])
    check_ruthos(root)
    write_highdicom(root)
