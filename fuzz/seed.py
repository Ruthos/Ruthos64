"""Seed the fuzz corpora: the DICOM samples for the file targets, bare
codestreams cut from them for the codec targets, and a sidecar."""
import pathlib
import shutil

root = pathlib.Path(__file__).resolve().parent
samples = root.parent / "testdata" / "pydicom"
corpus = root / "corpus"
for t in ("open_file", "decode_frame", "jpeg_codestream", "guard_headers", "anonymise", "sidecar", "profile_and_link"):
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
seeds = {
    "join": "ruthos://join?server=https%3A%2F%2Frelay.example.org%2Fteach&code=482913",
    "link": "ruthos://profile?url=https://relay.example.org/a.ruthosprofile",
    "ini": "[guided-review]\nserver = https://relay.example.org/teach\nallowed_servers = https://a.example.org, https://b.example.org\n",
    "unsigned": '{"ruthos_profile":1,"measurements":{"library":{}},"viewer":{"echo_length_unit":"cm","loupe":{"magnification":4}}}',
    "signed": '{"ruthos_profile":1,"issuer":{"server":"https://relay.example.org/teach","name":"n","by":"b"},'
    '"issued":"2026-11-04T09:00:00Z","expires":"2027-07-31T23:59:59Z","server":"https://relay.example.org/teach",'
    '"session":{"code":"482913"},"cases":["EQ-CARD-2026-014"],'
    '"sig":{"alg":"Ed25519","kid":"k","value":"' + "A" * 86 + '=="}}',
}
for name, text in seeds.items():
    (corpus / "profile_and_link" / name).write_text(text)
print("seeded", sum(1 for _ in corpus.rglob("*") if _.is_file()), "files")
