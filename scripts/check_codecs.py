#!/usr/bin/env python3
"""Compare the bundled C codecs in codecs.toml with upstream.

For each codec: the latest GitHub release, and the CVEs the National
Vulnerability Database lists for the recorded version. Also checks that the
versions recorded match the crates in Cargo.lock. Prints each finding; with
--open-issues, opens a GitHub issue (label "security") for each one that
has no open issue with the same title, using the gh CLI.
"""
import json
import os
import subprocess
import re
import sys
import tomllib
import urllib.request


def get(url, headers=None):
    req = urllib.request.Request(url, headers={"User-Agent": "ruthos-codec-check", **(headers or {})})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def version_tuple(v):
    return tuple(int(x) for x in re.findall(r"\d+", v)[:3])


def main():
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
            if latest and version_tuple(latest) > version_tuple(c["version"]):
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
            cpe = f"{c['cpe']}:{c['version']}:*:*:*:*:*:*:*"
            nvd = get(f"https://services.nvd.nist.gov/rest/json/cves/2.0?cpeName={cpe}")
            ids = [v["cve"]["id"] for v in nvd.get("vulnerabilities", [])]
            if ids:
                findings.append((
                    f"{c['name']} {c['version']} has known CVEs: {', '.join(ids[:5])}",
                    f"The National Vulnerability Database lists {len(ids)} CVE(s) affecting {c['name']} "
                    f"{c['version']}, which Ruthos bundles through {c['crate']}: {', '.join(ids)}.\n\n"
                    "Check each against Ruthos's use of the library and the decode guard, and update or patch.",
                ))
        except Exception as e:  # noqa: BLE001
            print(f"warning: NVD check for {c['name']} failed: {e}", file=sys.stderr)
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
