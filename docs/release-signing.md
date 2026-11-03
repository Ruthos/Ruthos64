# Release signing

How Ruthos releases are signed, what has to be set up once, and how a
release is cut. Windows is the platform in use today; macOS is fully
specified and wired into the workflow but switched off until the Apple
enrolment is done.

| Platform | Route | State |
|---|---|---|
| Windows | SignPath Foundation, Authenticode, RFC 3161 timestamp | Workflow ready; waiting for SignPath to accept the project |
| macOS | Apple Developer ID, hardened runtime, notarised and stapled | Workflow ready behind `MACOS_RELEASE`; enrolment and publisher name undecided |
| Linux | No platform signature; checksums and provenance only | Workflow ready behind `LINUX_RELEASE` |
| All | `SHA256SUMS.txt` and a GitHub build-provenance attestation | Active from the first tagged release |

Signing happens only in `.github/workflows/release.yml` on GitHub-hosted
runners, from a tagged commit, never on a developer's machine.

## Windows: SignPath Foundation

`ruthos.exe` carries a version resource (product name, description,
version, original file name) from `crates/ruthos/build.rs`, which the
Foundation requires; the release workflow fails if it is missing or the
version does not match the tag.

Order of work:

1. ~~Listener off by default.~~ Done.
2. ~~Version resource in `ruthos.exe`.~~ Done.
3. ~~Code signing policy and privacy statement in the README and release notes.~~ Done.
4. Publish v0.1.0-alpha unsigned from the release workflow, so the project is released in the form to be signed. (Tag pushed; the draft waits for review.)
5. Apply at <https://signpath.org/apply>, naming the repository `Ruthos/Ruthos64`, the workflow `.github/workflows/release.yml` and Mark Bowen as the approver of signing requests.
6. Once accepted: add the secret `SIGNPATH_API_TOKEN` to the `release` environment, and the repository variables `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG`. In SignPath, configure the artefact as a single PE file (`ruthos.exe`) and link the GitHub trusted build system.
7. Tag v0.1.1-alpha. The workflow builds, uploads the unsigned executable, and the signing job waits for approval of the `release` environment; SignPath then waits for approval of the signing request. After both, the workflow checks that `Get-AuthenticodeSignature` reports `Valid`, that the signer is SignPath Foundation and that the signature is timestamped, and fails otherwise.

## macOS: Developer ID and notarisation

Open decision: the publisher shown on macOS is either **Mark Bowen**
(individual enrolment, faster) or **Medicine Vet Equine Referrals Ltd**
(organisation enrolment with a D-U-N-S number, keeps the personal name off
the binary and allows other developers on the team). SignPath has no
macOS equivalent of its neutral publisher name. The Apple Developer
Program costs US$99 a year.

Once enrolled:

1. Create a Developer ID Application certificate, export it with its private key as a password-protected `.p12`, and store it base64-encoded as `APPLE_CERTIFICATE` with its password as `APPLE_CERTIFICATE_PASSWORD`, both in the `release` environment.
2. Create an App Store Connect API key with the Developer role; store the `.p8` contents as `APPLE_API_KEY`, and its key ID and issuer ID as `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`, in the `release` environment.
3. Set the repository variable `MACOS_RELEASE` to `true`.

The macOS job then builds both architectures, joins them with `lipo`,
bundles `Ruthos.app` with cargo-packager, imports the certificate into a
temporary keychain (deleted at the end of the job), signs with
`codesign --force --options runtime --timestamp` and the empty
`packaging/macos/entitlements.plist`, notarises a `ditto` zip with
`scripts/notarise.sh` (which fails on anything but Accepted and prints the
notary log), staples, builds a disk image and signs, notarises and staples
that too, and finally verifies with `spctl --assess --type execute -vv`
(the source must be "Notarized Developer ID") and `xcrun stapler validate`
on both. A failure at the verification step stops the release.

The application is not sandboxed and has no hardened-runtime exceptions.

If the certificate is ever exposed: revoke it in the Apple developer
portal, asking Apple to date the revocation from the moment of exposure
(timestamped signatures from before then stay valid), and revoke and
reissue the App Store Connect API key.

## Checksums and provenance

The last job of the release workflow writes `SHA256SUMS.txt` over every
artefact and runs `actions/attest-build-provenance` on them, then creates
the draft pre-release with `RELEASE_NOTES.md` as its text. Verify a
download with:

```
gh attestation verify <file> --repo Ruthos/Ruthos64
```

## Secrets and approvals

| Secret or setting | Where it lives | Who can use it |
|---|---|---|
| Windows signing key | SignPath Foundation's hardware security module | SignPath, after the maintainer approves each request |
| `SIGNPATH_API_TOKEN` | Environment `release` | Jobs in that environment |
| `APPLE_CERTIFICATE` and its password | Environment `release` | Jobs in that environment |
| `APPLE_API_KEY`, key ID and issuer ID | Environment `release` | Jobs in that environment |
| Environment `release` protection | Repository settings | Deployments only from tags matching `v*`, after the maintainer approves the run |
| Tag rule | Repository ruleset "Release tags" | Only repository and organisation admins can create, move or delete `v*` tags |

A pull request from a fork never sees these secrets: GitHub withholds
secrets from fork workflows, and the environment accepts only `v*` tags.

## Cutting a release

1. Update `RELEASE_NOTES.md` and the workspace version in `Cargo.toml` (and `Packager.toml`), so `ruthos.exe` reports the same version as the tag.
2. Push the commit, wait for CI to pass, then `git tag v0.1.1-alpha && git push origin v0.1.1-alpha`.
3. Approve the `release` environment run in GitHub Actions, then the signing request in SignPath.
4. Review the draft release: download each artefact on a clean machine, check `SHA256SUMS.txt` and `gh attestation verify`, then publish.

## Acceptance

Against the draft release, before publishing:

1. `Get-AuthenticodeSignature` reports Valid on `ruthos.exe`, the signer is SignPath Foundation, and the signature is timestamped.
2. `ruthos.exe` starts on a Windows 11 PC with Smart App Control in enforce mode, which refuses the unsigned build of the same commit.
3. File properties of `ruthos.exe` show the product name Ruthos and the release version.
4. On a Mac, `spctl` reports "Notarized Developer ID" and `xcrun stapler validate` succeeds on the application and the disk image.
5. The application downloaded through a browser opens with only the standard first-launch confirmation, also on a Mac that is offline.
6. Every artefact matches `SHA256SUMS.txt` and `gh attestation verify` succeeds for each.
7. A run from a branch or a fork's pull request cannot reach the `release` environment, and a tag push waits for approval before any signing step.
8. The README and the release page carry the code signing policy and the privacy statement.
