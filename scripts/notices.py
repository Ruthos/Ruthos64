#!/usr/bin/env python3
"""Third-party notices (Third-Party Notices Specification).

    notices.py build --target <triple> [<triple> ...] --out <folder>
        Write THIRD_PARTY_LICENSES.html and .txt for one platform.
    notices.py copyrights
        Refresh licenses/crate-copyrights.tsv. Many crates do not package a
        licence file, so cargo-about can give only a licence's bare template.
        This reads each one's copyright line from its repository, for review.
    notices.py check [--target <triple>]
        The checks CI runs: about.toml and deny.toml agree, each file in
        licenses/ matches its vendored original, and with --target, every crate
        in that platform's dependency graph is in its notices.

cargo-about reports what a crate declares. It does not see the C library inside
a -sys crate or files Ruthos embeds, so those are listed in BUNDLED.
"""
import argparse
import base64
import html
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COPYRIGHTS = ROOT / "licenses" / "crate-copyrights.tsv"
TARGETS = [
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
]
IJG = "This software is based in part on the work of the Independent JPEG Group."
PREFACE = f"""Ruthos is licensed under the GNU Lesser General Public License v3.0; the text is in
the LICENSE file beside this one. Ruthos is not a medical device and is not for
diagnostic use.

{IJG}

Acknowledgements. Ruthos was created by Mark Bowen. It is built on DICOM-rs by Eduardo Pinho and contributors,
including Paul Knopf (upper-layer networking) and Peter Evers (pixel data), which
provides its DICOM parsing, pixel decoding and networking. The interface is built
with egui by Emil Ernerfeldt and contributors, and rendering uses wgpu. Image
decoding uses libjpeg-turbo, OpenJPEG and CharLS. Ruthos's workflow, icons, colour
tables and presets follow Horos, its interface uses the Inter typeface and Phosphor
icons, and its test files come from pydicom.

OpenH264 Video Codec provided by Cisco Systems, Inc. The encoder is not part of
this package: Ruthos fetches Cisco's own binary from Cisco, only if the user agrees,
the first time a movie is exported. The optional AI tool likewise fetches SAM 2.1
(Apache 2.0, Meta) and Microsoft's ONNX Runtime and DirectML on request."""

# Title, what it is and why it is here, the file holding its licence, and which
# locked crate vendors that file (crate, path inside it), if one does.
BUNDLED = [
    ("libjpeg-turbo", f"Modified BSD and IJG licences, through turbojpeg-sys. {IJG}",
     "licenses/libjpeg-turbo-LICENSE.md", ("turbojpeg-sys", "libjpeg-turbo/LICENSE.md")),
    ("libjpeg-turbo: the Independent JPEG Group's notice", "The IJG licence terms, from README.ijg.",
     "licenses/libjpeg-turbo-README.ijg", ("turbojpeg-sys", "libjpeg-turbo/README.ijg")),
    ("OpenJPEG", "BSD 2-Clause, through openjpeg-sys.",
     "licenses/openjpeg-LICENSE", ("openjpeg-sys", "vendor/LICENSE")),
    ("CharLS", "BSD 3-Clause, through charls-sys.",
     "licenses/charls-LICENSE.md", ("charls-sys", "charls/LICENSE.md")),
    ("SQLite", "Public domain, through libsqlite3-sys. Credited as a courtesy.",
     "licenses/sqlite-NOTICE.txt", None),
    ("OpenH264: source licence", "BSD 2-Clause. The binary is not in this package.",
     "licenses/openh264-LICENSE", ("openh264-sys2", "upstream/LICENSE")),
    ("OpenH264: Cisco's binary licence", "The terms of the binary Ruthos fetches from Cisco with the user's consent.",
     "licenses/openh264-BINARY_LICENSE.txt", None),
    ("Inter typeface", "SIL Open Font Licence 1.1.", "assets/fonts/INTER-OFL.txt", None),
    ("Phosphor icons", "The icon font's licence.", "assets/fonts/PHOSPHOR-LICENSE", None),
    ("Horos", "Icons, colour tables, annotation defaults and presets. LGPL-3.0.",
     "licenses/horos-NOTICE.txt", None),
]


def run(*args):
    done = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, encoding="utf8")
    if done.returncode:
        # cargo-about's own reason is what a contributor needs to see.
        raise SystemExit(f"::error::{args[0]} {args[1]} failed\n{done.stderr[-4000:]}")
    return done.stdout


def about_command():
    custom = os.environ.get("CARGO_ABOUT")
    return [custom] if custom else ["cargo", "about"]


def bundled_text():
    parts = []
    for title, note, path, _ in BUNDLED:
        parts.append((title, note, (ROOT / path).read_text(encoding="utf8", errors="replace")))
    return parts


def graph(target):
    """(name, version) of every crate in a platform's normal dependency graph."""
    tree = run("cargo", "tree", "-p", "ruthos", "--target", target, "-e", "normal", "--prefix", "none",
               "--locked", "--format", "{p}")
    crates = set()
    for line in tree.splitlines():
        parts = line.split()
        if len(parts) >= 2 and "(" not in parts[0] and not parts[0].startswith("ruthos"):
            crates.add((parts[0], parts[1].lstrip("v")))
    return crates


def packages():
    meta = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked"))
    return {(p["name"], p["version"]): p for p in meta["packages"]}


def own_copyright(package):
    """True when the crate ships a licence file carrying a real copyright line."""
    folder = Path(package["manifest_path"]).parent
    for f in folder.iterdir():
        if f.is_file() and re.match(r"(?i)(licen[sc]e|copying|notice|copyright)", f.name):
            for line in f.read_text(encoding="utf8", errors="replace").splitlines():
                if is_copyright(line):
                    return True
    return False


def is_copyright(line):
    line = line.strip()
    return (
        re.match(r"(?i)copyright\s*(\(c\)|©|\d{4})", line) is not None
        and not re.search(r"[<\[{]|yyyy|copyright (owner|holder)", line, re.I)
    )


def upstream_lines(repository):
    """Copyright lines from the licence files at the root of a GitHub repository."""
    match = re.search(r"github\.com[/:]([^/]+/[^/#]+)", repository or "")
    if not match:
        return []
    repo = match.group(1).removesuffix(".git")
    try:
        names = run("gh", "api", f"repos/{repo}/contents/", "--jq", ".[].name").split()
    except SystemExit:
        return []
    found = []
    for name in names:
        if not re.match(r"(?i)(licen[sc]e|copying|notice)", name):
            continue
        try:
            body = run("gh", "api", f"repos/{repo}/contents/{name}", "--jq", ".content")
        except SystemExit:
            continue
        text = base64.b64decode(body).decode("utf8", "replace")
        found += [l.strip() for l in text.splitlines() if is_copyright(l)]
    return list(dict.fromkeys(found))


def needing_lines():
    known = packages()
    wanted = {}
    for target in TARGETS:
        for key in graph(target):
            package = known.get(key)
            if package and not own_copyright(package):
                wanted[key[0]] = package
    return wanted


def refresh_copyrights():
    rows = {}
    cached = {}
    for name, package in sorted(needing_lines().items()):
        repository = package.get("repository") or ""
        lines = cached.setdefault(repository, upstream_lines(repository))
        if lines:
            rows[name] = (" / ".join(lines[:3]), "repository")
        else:
            authors = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in package.get("authors", []))
            line = f"Copyright the {package['name']} authors" + (f": {authors}" if authors else "")
            rows[name] = (line, "manifest")
    text = "# Copyright lines for crates that package no licence file, by notices.py copyrights.\n"
    text += "# crate<TAB>copyright<TAB>where it came from\n"
    text += "".join(f"{n}\t{l}\t{w}\n" for n, (l, w) in sorted(rows.items()))
    COPYRIGHTS.write_text(text, encoding="utf8", newline="\n")
    by = [w for _, w in rows.values()]
    print(len(rows), "crates:", by.count("repository"), "from repositories,", by.count("manifest"), "from manifests")


def read_copyrights():
    rows = {}
    if COPYRIGHTS.is_file():
        for line in COPYRIGHTS.read_text(encoding="utf8").splitlines():
            if line and not line.startswith("#"):
                name, text, _ = line.split("\t")
                rows[name] = text
    return rows


def copyright_section(targets):
    table = read_copyrights()
    names = sorted({n for t in targets for n, _ in graph(t)} & set(table))
    return "\n".join(f"{n}: {table[n]}" for n in names)


def build(targets, out):
    """One set of notices for a package; a universal binary names both targets."""
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        files = {}
        for template, name in (("about.hbs", "t.html"), ("about-txt.hbs", "t.txt")):
            dest = Path(tmp) / name
            run(*about_command(), "generate", "--fail", "--locked", "-m", "crates/ruthos/Cargo.toml",
                *[a for t in targets for a in ("--target", t)], template, "-o", str(dest))
            files[name] = dest.read_text(encoding="utf8")
    parts = bundled_text()
    lines = copyright_section(targets)
    title = "Copyright notices of crates that package no licence file"
    note = "These apply with the licence text each crate is listed under above."
    parts.insert(0, (title, note, lines))
    page = files["t.html"]
    page = page.replace("<!-- PREFACE -->", "".join(f"<p>{html.escape(p)}</p>" for p in PREFACE.split("\n\n")))
    page = page.replace("<!-- BUNDLED -->", "<h2>C libraries, fonts and assets</h2>" + "".join(
        f"<h3>{html.escape(t)}</h3><p>{html.escape(n)}</p><pre>{html.escape(x)}</pre>" for t, n, x in parts))
    bar = "=" * 80
    text = files["t.txt"].replace("@@PREFACE@@", PREFACE).replace(
        "@@BUNDLED@@", "C LIBRARIES, FONTS AND ASSETS\n" + "".join(f"\n{bar}\n{t}\n{n}\n\n{x}\n" for t, n, x in parts))
    (out / "THIRD_PARTY_LICENSES.html").write_text(page, encoding="utf8")
    (out / "THIRD_PARTY_LICENSES.txt").write_text(text, encoding="utf8")
    print("wrote", out, "for", ", ".join(targets))


def quoted_list(text, key):
    match = re.search(rf"^{key} = \[(.*?)^\]", text, re.S | re.M)
    if not match:
        raise SystemExit(f"no {key} list")
    return set(re.findall(r'"([^"]+)"', match.group(1)))


def check_lists():
    accepted = quoted_list((ROOT / "about.toml").read_text(encoding="utf8"), "accepted")
    allowed = quoted_list((ROOT / "deny.toml").read_text(encoding="utf8"), "allow")
    if accepted != allowed:
        return [f"about.toml and deny.toml differ: {sorted(accepted ^ allowed)}"]
    return []


def registry_source(crate):
    """The folder of the locked version of a crate, fetched if need be."""
    meta = run("cargo", "metadata", "--format-version", "1", "--locked")
    for package in json.loads(meta)["packages"]:
        if package["name"] == crate:
            return Path(package["manifest_path"]).parent
    return None


def check_vendored():
    problems = []
    for _, _, path, origin in BUNDLED:
        if not (ROOT / path).is_file():
            problems.append(f"{path} is missing")
            continue
        if origin is None:
            continue
        crate, inner = origin
        source = registry_source(crate)
        original = source / inner if source else None
        if original is None or not original.is_file():
            problems.append(f"{crate} no longer has {inner}")
        elif original.read_bytes().replace(b"\r\n", b"\n") != (ROOT / path).read_bytes().replace(b"\r\n", b"\n"):
            problems.append(f"{path} differs from {crate}'s {inner}: review the new licence")
    return problems


def check_copyrights():
    table = read_copyrights()
    missing = sorted(set(needing_lines()) - set(table))
    return [f"no copyright line for {n}: run notices.py copyrights and review it" for n in missing]


def check_complete(targets):
    """Every crate in the platform's normal dependency graph is in its notices."""
    crates = {c for t in targets for c in graph(t)}
    with tempfile.TemporaryDirectory() as tmp:
        build(targets, Path(tmp))
        text = (Path(tmp) / "THIRD_PARTY_LICENSES.txt").read_text(encoding="utf8")
    missing = sorted(f"{n} {v}" for n, v in crates if f"  {n} {v}\n" not in text)
    print(len(crates), "crates in the graph for", ", ".join(targets))
    return [f"not in the notices: {c}" for c in missing]


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build")
    b.add_argument("--target", required=True, nargs="+")
    b.add_argument("--out", required=True, type=Path)
    sub.add_parser("copyrights")
    c = sub.add_parser("check")
    c.add_argument("--target", nargs="+")
    args = parser.parse_args()
    if args.command == "build":
        build(args.target, args.out)
        return
    if args.command == "copyrights":
        refresh_copyrights()
        return
    problems = check_lists() + check_vendored() + check_copyrights()
    if args.target:
        problems += check_complete(args.target)
    for p in problems:
        print("::error::" + p)
    if problems:
        sys.exit(1)
    print("notices are complete")


if __name__ == "__main__":
    main()
