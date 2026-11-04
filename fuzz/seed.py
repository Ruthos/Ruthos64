"""Seed the fuzz corpora: the DICOM samples for the file targets, bare
codestreams cut from them for the codec targets, and a sidecar."""
import pathlib
import shutil

root = pathlib.Path(__file__).resolve().parent
samples = root.parent / "testdata" / "pydicom"
corpus = root / "corpus"
for t in ("open_file", "decode_frame", "jpeg_codestream", "guard_headers", "anonymise", "sidecar"):
    (corpus / t).mkdir(parents=True, exist_ok=True)
for f in samples.glob("*.dcm"):
    for t in ("open_file", "decode_frame", "anonymise"):
        dest = corpus / t / f.name
        if not dest.exists():
            shutil.copy(f, dest)
    data = f.read_bytes()
    for marker in (b"\xff\xd8\xff", b"\xff\x4f\xff\x51"):
        start = data.find(marker)
        if start > 0:
            for t in ("jpeg_codestream", "guard_headers"):
                (corpus / t / f"{f.stem}-{start}").write_bytes(data[start:start + 65536])
(corpus / "sidecar" / "seed.json").write_text(
    '{"format":1,"reference":"X","name":"n","title":"t","answer_key":false,'
    '"measurements":[],"calibrations":[],"key_images":[]}'
)
print("seeded", sum(1 for _ in corpus.rglob("*") if _.is_file()), "files")
