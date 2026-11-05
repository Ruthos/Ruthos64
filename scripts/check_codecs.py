#!/usr/bin/env python3
"""Compare the bundled C codecs in codecs.toml with upstream.

For each codec: the latest GitHub release, the CVEs the National Vulnerability
Database lists for the recorded version or has recently published under the
codec's name, and upstream commits since the last review that look like
memory-safety fixes, since many of those never get a CVE. Also checks that the
versions recorded match the crates in Cargo.lock. Prints each finding; with
--open-issues, opens a GitHub issue (label "security") for each one that
has no open issue with the same title, using the gh CLI.

With --verify-vendor it does none of that: it compares vendored C with the
upstream tag and exits non-zero on any difference codecs.toml does not record.
"""
import datetime
import hashlib
import io
import json
import os
import subprocess
import re
import sys
import tarfile
import time
import tomllib
import urllib.parse
import urllib.request
from pathlib import Path

# Commit subjects worth a person's time. Noise is cheap; a missed fix is not.
SAFETY = re.compile(
    r"overflow|overrun|out.of.bounds|\boob\b|use.after|double.free|heap|stack|null.?(pointer|deref)"
    r"|crash|segfault|fuzz|sanitiz|\basan\b|\bubsan\b|invalid (read|write|memory)|bounds? check"
    r"|cve-|security|vulnerab",
    re.I,
)


def get(url, headers=None):
    req = urllib.request.Request(url, headers={"User-Agent": "ruthos-codec-check", **(headers or {})})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def version_tuple(v):
    return tuple(int(x) for x in re.findall(r"\d+", v)[:3])


def nvd_ids(query):
    time.sleep(7)  # NVD allows five requests in thirty seconds without a key.
    nvd = get(f"https://services.nvd.nist.gov/rest/json/cves/2.0?{query}")
    return [v["cve"]["id"] for v in nvd.get("vulnerabilities", [])]


def cve_ids(c):
    """CVEs for this version, and recent ones under the name: a new record often
    has no version data yet, so a search by version alone does not find it."""
    ids = nvd_ids("cpeName=" + urllib.parse.quote(f"{c['cpe']}:{c['version']}:*:*:*:*:*:*:*"))
    now = datetime.datetime.now(datetime.timezone.utc)
    since = now - datetime.timedelta(days=119)  # NVD's widest range is 120 days.
    stamp = "%Y-%m-%dT%H:%M:%S.000Z"
    recent = urllib.parse.urlencode({
        "keywordSearch": c["name"],
        "pubStartDate": since.strftime(stamp),
        "pubEndDate": now.strftime(stamp),
    })
    ids += nvd_ids(recent)
    return sorted(set(ids) - set(c.get("reviewed_cves", [])))


def unreviewed_fixes(c, headers):
    """Upstream commits since the last review whose subjects suggest a safety fix."""
    base = urllib.parse.quote(c["reviewed_commit"], safe="")
    compare = get(f"https://api.github.com/repos/{c['github']}/compare/{base}...HEAD", headers)
    found = []
    for commit in compare.get("commits", []):
        subject = commit["commit"]["message"].splitlines()[0]
        if SAFETY.search(subject) and not subject.startswith("Merge "):
            found.append(f"- {commit['html_url']} {subject}")
    return found


def verify_vendor():
    with open("codecs.toml", "rb") as f:
        codecs = tomllib.load(f)["codec"]
    problems = []
    for c in codecs:
        if "vendored" not in c:
            continue
        url = f"https://codeload.github.com/{c['github']}/tar.gz/refs/tags/{c['tag']}"
        req = urllib.request.Request(url, headers={"User-Agent": "ruthos-codec-check"})
        with urllib.request.urlopen(req, timeout=120) as r:
            archive = tarfile.open(fileobj=io.BytesIO(r.read()), mode="r:gz")
        upstream = {}
        for member in archive.getmembers():
            if member.isfile():
                upstream[member.name.split("/", 1)[1]] = archive.extractfile(member).read()
        root = Path(c["vendored"])
        patched = c.get("patched", {})
        files = [p for p in root.rglob("*") if p.is_file()]
        for path in files:
            name = path.relative_to(root).as_posix()
            ours = path.read_bytes().replace(b"\r\n", b"\n")
            if name in patched:
                if hashlib.sha256(ours).hexdigest() != patched[name]:
                    problems.append(f"{name} does not match the SHA-256 in codecs.toml")
            elif name not in upstream:
                problems.append(f"{name} is not in {c['name']} {c['tag']}")
            elif upstream[name].replace(b"\r\n", b"\n") != ours:
                problems.append(f"{name} differs from {c['name']} {c['tag']}")
        for name in patched:
            if not (root / name).is_file():
                problems.append(f"{name} is recorded as patched but is missing")
        print(f"{c['name']}: {len(files)} files against {c['tag']}, {len(patched)} patched")
    for p in problems:
        print("::error::" + p)
    if problems:
        sys.exit(1)


def main():
    if "--verify-vendor" in sys.argv:
        verify_vendor()
        return
    with open("codecs.toml", "rb") as f:
        codecs = tomllib.load(f)["codec"]
    lock = open("Cargo.lock", encoding="utf-8").read()
    token = os.environ.get("GITHUB_TOKEN", "")
    gh_headers = {"Authorization": f"Bearer {token}"} if token else {}
    findings = []
    for c in codecs:
        # The crate in Cargo.lock must be the one recorded.
        m = re.search(r'name = "%s"\nversion = "([^"]+)"' % re.escape(c["crate"]), lock)
        if m and m.group(1) != c["crate_version"]:
            findings.append((
                f"codecs.toml is out of date for {c['crate']}",
                f"Cargo.lock has {c['crate']} {m.group(1)} but codecs.toml records {c['crate_version']}. "
                f"Check which {c['name']} version it bundles and update codecs.toml.",
            ))
        # The latest upstream release.
        try:
            rel = get(f"https://api.github.com/repos/{c['github']}/releases/latest", gh_headers)
            latest = rel.get("tag_name", "")
            known = max(version_tuple(c["version"]), version_tuple(c.get("reviewed_release", "0")))
            if latest and version_tuple(latest) > known:
                findings.append((
                    f"{c['name']} {latest.lstrip('v')} is available (Ruthos bundles {c['version']})",
                    f"{c['name']} {latest} was released upstream: {rel.get('html_url', '')}\n\n"
                    f"Ruthos bundles {c['version']} through {c['crate']} {c['crate_version']}. "
                    f"Check the release notes and {c['advisories']} for security fixes. If there are any and "
                    f"{c['crate']} has not picked the release up, patch it in with a [patch.crates-io] fork.",
                ))
        except Exception as e:  # noqa: BLE001 - report and carry on
            print(f"warning: release check for {c['name']} failed: {e}", file=sys.stderr)
        # CVEs listed for this exact version.
        try:
            ids = cve_ids(c)
            if ids:
                findings.append((
                    f"{c['name']} {c['version']} has known CVEs: {', '.join(ids[:5])}",
                    f"The National Vulnerability Database lists {len(ids)} CVE(s) affecting {c['name']} "
                    f"{c['version']}, which Ruthos bundles through {c['crate']}: {', '.join(ids)}.\n\n"
                    "Check each against Ruthos's use of the library and the decode guard, and update or patch.",
                ))
        except Exception as e:  # noqa: BLE001
            print(f"warning: NVD check for {c['name']} failed: {e}", file=sys.stderr)
        # Fixes that have no CVE: a release may never carry them.
        try:
            fixes = unreviewed_fixes(c, gh_headers)
            if fixes:
                since = c["reviewed_commit"][:9]
                findings.append((
                    f"{c['name']}: upstream fixes to review since {since}",
                    f"These commits on {c['github']} since {since} look like memory-safety fixes. They may "
                    "have no CVE and may not be in any release.\n\n" + "\n".join(fixes[:60]) + "\n\n"
                    "Check each against the code Ruthos builds, apply what applies, and move "
                    "reviewed_commit forward in codecs.toml.",
                ))
        except Exception as e:  # noqa: BLE001
            print(f"warning: commit check for {c['name']} failed: {e}", file=sys.stderr)
    for title, _ in findings:
        print(f"- {title}")
    if "--open-issues" not in sys.argv:
        return
    listed = subprocess.run(
        ["gh", "issue", "list", "--state", "open", "--limit", "200", "--json", "title"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    open_titles = {i["title"] for i in json.loads(listed)}
    for title, body in findings:
        if title in open_titles:
            print(f"  already open: {title}")
            continue
        subprocess.run(["gh", "issue", "create", "--title", title, "--body", body, "--label", "security"], check=False)


if __name__ == "__main__":
    main()
