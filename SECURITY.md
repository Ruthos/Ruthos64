# Security

Ruthos is used in clinical practice, so a security problem in it matters. Thank you for taking the time to report one.

## Reporting a vulnerability

Please report it privately, not in a public issue:

1. Go to the repository's **Security** tab and choose **Report a vulnerability** (GitHub's private vulnerability reporting).
2. Describe the problem, the version you tested, and how to reproduce it. A file that triggers it is the most useful thing you can send; attach it to the private report, never to a public issue.

We acknowledge every report within **five working days**, keep you informed while we work on it, and credit you in the release notes unless you would rather not be named.

## What happens next

A security fix is released as a new patch version with a note in the release notes. The note says what was fixed and which versions are affected, without describing how to exploit it. If the problem is in one of the C libraries Ruthos bundles (libjpeg-turbo, OpenJPEG or CharLS), we also report it privately to that project and ship a patched copy until a fixed release reaches the crate that bundles it.

## Supported versions

Ruthos is in alpha. Only the latest release receives security fixes.

## How Ruthos protects itself

- Every image passes a guard before any decoder sees it: its size and layout are checked, and the codec's own header must agree with the DICOM attributes (see `crates/ruthos-dicom/src/guard.rs`).
- Everything that parses bytes Ruthos did not create is fuzzed in CI, including the bundled C decoders under AddressSanitizer (`fuzz/`).
- Dependencies are checked against the RustSec advisory database on every change and nightly (`deny.toml`), and the bundled C libraries against their upstream releases and the National Vulnerability Database (`codecs.toml`).
- The DICOM listener is off by default, and when on it accepts only the devices listed in Preferences.
