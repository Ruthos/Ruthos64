#!/usr/bin/env python3
"""Keep the version, download size and dates that appear outside the code in step.

The version lives in Cargo.toml. It is repeated in Packager.toml and in the
release notes (the heading and the `gh attestation verify` line), and the
site names the published release in its download links, its version line,
its download table, its install steps and its verify line, with the zip's
size in two places and the release date in the sitemap. These drift when
edited by hand, and the download size in particular was wrong for five
releases.

    stamp_release.py check [--online]
        Exits non-zero when Cargo.toml, Packager.toml and RELEASE_NOTES.md
        disagree, or when the site names more than one version or two sizes.
        With --online it also asks GitHub for the release the site names and
        checks the size against the zip and the sitemap date against the
        release date. CI runs this.

    stamp_release.py stamp vX.Y.Z
        Rewrites the site for that published release: links, version,
        size (from the zip on GitHub, to the nearest MB) and sitemap date.
        Run it after a release is published, then commit site/.

The site may lag Cargo.toml: between the version bump and the release the
code is already the next version while the site still offers the last one.
"""
import argparse
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "Ruthos/Ruthos64"
ZIP = "Ruthos-{v}-win64.zip"
VERSION = r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?"


def read(name):
    return (ROOT / name).read_text(encoding="utf-8")


def one(pattern, text, what):
    found = sorted(set(re.findall(pattern, text)))
    if len(found) != 1:
        raise SystemExit(f"{what}: expected one value, found {found or 'none'}")
    return found[0]


def code_version():
    return one(r'(?m)^version = "(' + VERSION + r')"', read("Cargo.toml"), "Cargo.toml version")


def check_code(v):
    errors = []
    packager = one(r'(?m)^version = "(' + VERSION + r')"', read("Packager.toml"), "Packager.toml version")
    if packager != v:
        errors.append(f"Packager.toml says {packager}, Cargo.toml says {v}")
    notes = read("RELEASE_NOTES.md")
    heading = one(r"(?m)^# Ruthos (" + VERSION + r")\s*$", notes, "release notes heading")
    if heading != v:
        errors.append(f"RELEASE_NOTES.md heading says {heading}, Cargo.toml says {v}")
    verify = one(r"gh attestation verify Ruthos-(" + VERSION + r")-win64\.zip", notes, "release notes verify line")
    if verify != v:
        errors.append(f"RELEASE_NOTES.md verify line names {verify}, Cargo.toml says {v}")
    return errors


def site_state():
    html = read("site/index.html")
    version = one(r"releases/download/v(" + VERSION + r")/", html, "site download links")
    for pattern, what in [
        (r"Ruthos-(" + VERSION + r")-win64\.zip", "site zip names"),
        (r"Version (" + VERSION + r") ·", "site version line"),
        (r">(" + VERSION + r") available<", "site download table"),
    ]:
        if one(pattern, html, what) != version:
            raise SystemExit(f"{what} does not name {version}")
    size = int(one(r"(\d+) MB zip", html, "site size in the version line"))
    if int(one(r"\((\d+) MB\)", html, "site size in the download table")) != size:
        raise SystemExit("the site gives two different sizes for the zip")
    lastmod = one(r"<lastmod>(\d{4}-\d{2}-\d{2})</lastmod>", read("site/sitemap.xml"), "sitemap lastmod")
    return version, size, lastmod


def release(tag):
    url = f"https://api.github.com/repos/{REPO}/releases/tags/{tag}"
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=30) as r:
        data = json.load(r)
    name = ZIP.format(v=tag.lstrip("v"))
    asset = next((a for a in data["assets"] if a["name"] == name), None)
    if asset is None:
        raise SystemExit(f"{tag} has no asset {name}")
    return asset["size"], data["published_at"][:10]


def megabytes(size):
    return round(size / 1_000_000)


def check(online):
    v = code_version()
    errors = check_code(v)
    site_version, size, lastmod = site_state()
    if online:
        actual, date = release("v" + site_version)
        if megabytes(actual) != size:
            errors.append(f"the site says {size} MB but v{site_version} is {actual} bytes ({megabytes(actual)} MB)")
        if date != lastmod:
            errors.append(f"sitemap lastmod is {lastmod} but v{site_version} was published {date}")
    for e in errors:
        print("error:", e)
    print(f"code {v}; site offers {site_version}, {size} MB, dated {lastmod}" + (" (checked online)" if online else ""))
    return 1 if errors else 0


def stamp(tag):
    new = tag.lstrip("v")
    old, _, _ = site_state()
    size, date = release(tag)
    html = read("site/index.html")
    html = html.replace(old, new)
    html = re.sub(r"\d+ MB zip", f"{megabytes(size)} MB zip", html)
    html = re.sub(r"\(\d+ MB\)", f"({megabytes(size)} MB)", html)
    (ROOT / "site/index.html").write_text(html, encoding="utf-8")
    sitemap = re.sub(r"<lastmod>\d{4}-\d{2}-\d{2}</lastmod>", f"<lastmod>{date}</lastmod>", read("site/sitemap.xml"))
    (ROOT / "site/sitemap.xml").write_text(sitemap, encoding="utf-8")
    print(f"site: {old} -> {new}, {megabytes(size)} MB ({size} bytes), {date}")
    return check(online=True)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    c = sub.add_parser("check")
    c.add_argument("--online", action="store_true", help="also compare the site with the release on GitHub")
    s = sub.add_parser("stamp")
    s.add_argument("tag", help="a published release, such as v0.2.0-beta.1")
    args = p.parse_args()
    if args.command == "check":
        return check(args.online)
    return stamp(args.tag)


if __name__ == "__main__":
    sys.exit(main())
