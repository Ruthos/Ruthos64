# SignPath Foundation application: prepared answers

Answers for the form at <https://signpath.org/apply>, checked against the
Foundation's conditions at <https://signpath.org/terms> on 3 November 2026.
Copy them in; adjust anything that is no longer true.

## Before applying

| Condition | Where Ruthos stands |
|---|---|
| OSI-approved licence, no commercial dual licensing | LGPL-3.0-only for all Ruthos code. Bundled Horos artwork is LGPL-3.0, Inter and Phosphor fonts are OFL and MIT. |
| No proprietary components | None. The codecs (OpenJPEG, CharLS, libjpeg-turbo) are open source and statically linked; no unsigned DLLs ship. |
| Actively maintained | Commits most days since October 2026. |
| Already released in the form to be signed | Done: v0.1.0-alpha, a zip containing `ruthos.exe` built by the release workflow, published 3 November 2026. |
| Functionality described on the download page | Done: the release notes and <https://ruthos.github.io/Ruthos64/> (live). |
| Uninstallation instructions | On the site and in the README: delete the folder; optionally the data and settings folders. |
| Code signing policy on the home page and download page | README, the site (`#signing`) and the release notes. |
| Team roles | Committers and reviewers: Mark Bowen. Approvers: Mark Bowen. |
| Privacy statement | The no-transfer statement; true because the listener is off by default. |
| Metadata set and enforced | `ruthos.exe` carries product name Ruthos and the release version; the workflow checks them and the artifact configuration restricts the product name. |
| Built from source in a verifiable way | GitHub Actions on GitHub-hosted runners from a tagged commit, with build-provenance attestations. |
| Manual approval of every release | The `release` environment needs Mark's approval, then the signing request needs approval in SignPath. |
| **Multi-factor authentication for every team member, on SignPath and on the repository** | **Confirm two-factor authentication is on for the GitHub account `imbowen1973`**, and use it when creating the SignPath account. Consider requiring it for the Ruthos organisation (Settings > Authentication security). |

## Form answers

**Project name:** Ruthos

**Repository:** https://github.com/Ruthos/Ruthos64

**Homepage / download page:** https://ruthos.github.io/Ruthos64/ (downloads on https://github.com/Ruthos/Ruthos64/releases)

**Licence:** LGPL-3.0-only

**Short description:**
Ruthos is a free, open-source DICOM viewer and patient archive for
Windows, inspired by Horos and written in Rust. It imports and receives
medical images, shows them in a GPU-accelerated 2D viewer with calibrated
measurements, writes reports to PDF and makes anonymised teaching copies.
It is aimed at veterinary practices and teaching institutions that use
Horos on the Mac and need the same workflow on Windows.

**Why code signing is needed:**
Clinic and university PCs commonly run Windows 11 with Smart App Control
or managed application control, which refuse unsigned executables
outright, and institutional IT policies require signed software with
verifiable provenance before installation on managed machines. Signing
lets the people the project is for run it at all.

**What will be signed:** One file, `ruthos.exe` (a statically linked
64-bit Windows executable), delivered in a zip. Artifact configuration:
`packaging/signpath/artifact-configuration.xml`.

**Build system:** GitHub Actions, workflow `.github/workflows/release.yml`,
GitHub-hosted `windows-latest` runner, triggered by a `v*` tag. The
unsigned executable is uploaded as a workflow artefact and submitted with
`signpath/github-action-submit-signing-request@v3`.

**Release frequency:** Alpha releases every few weeks.

**Maintainer and approver:** Mark Bowen (GitHub `imbowen1973`).

**Network behaviour:** None by default. The user can enable a DICOM
listener (receives images from the user's own devices) and initiate DICOM
connections. No telemetry, no update checks, no data sent to the
developers or third parties.

## After acceptance

1. In SignPath: create the project with the artifact configuration above, a signing policy (release signing) with Mark Bowen as approver, and link the GitHub trusted build system to `Ruthos/Ruthos64`.
2. In GitHub, environment `release`: add the secret `SIGNPATH_API_TOKEN` (a SignPath CI user's token).
3. In GitHub, repository variables: `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`, `SIGNPATH_SIGNING_POLICY_SLUG`.
4. Bump the version to the next alpha version in `Cargo.toml` and `Packager.toml`, update `RELEASE_NOTES.md`, push, wait for CI, and tag it (for example `v0.1.4-alpha`).
5. Approve the environment run in GitHub Actions and the request in SignPath; the workflow fails unless the signature is Valid, from SignPath Foundation, and timestamped.
