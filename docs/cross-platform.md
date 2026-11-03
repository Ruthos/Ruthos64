# Cross-platform

Ruthos targets Windows 10 version 1809 and later on x86-64, macOS 11 Big
Sur and later as a universal binary, and Linux on x86-64 as a by-product.
Windows is where it is built and used today; this page records what is in
place for the other two so that nothing is assumed about Windows by
accident.

## What differs per platform

| Concern | Windows | macOS | Linux |
|---|---|---|---|
| Graphics | DirectX 12, then Vulkan, OpenGL | Metal, OpenGL | Vulkan, OpenGL |
| `RUTHOS_RENDERER=software` | WARP | ignored with a log line | llvmpipe |
| Interface font | Segoe UI | the system font from `/System/Library/Fonts` | bundled Inter |
| Data folder | `%LOCALAPPDATA%\Ruthos\Ruthos Data` | `~/Library/Application Support/Ruthos/Ruthos Data` | `~/.local/share/ruthos/Ruthos Data` |
| Settings | `%APPDATA%\Ruthos\settings.json` | `~/Library/Application Support/Ruthos/settings.json` | `~/.config/ruthos/settings.json` |
| Opening a PDF | `start` | `open` | `xdg-open` |
| Shortcut text | `Ctrl+S` | `⌘S` | `Ctrl+S` |
| Files opened from the system | command-line arguments | `application:openURLs:` from Finder and the Dock | command-line arguments |
| Menus | toolbars in the windows | the native menu bar, from the same definition | toolbars in the windows |

All of it lives in `crates/ruthos-ui/src/platform/` (shortcut text,
opening files, the open-file queue, the menu bar as data, and under
`macos.rs` the Finder hook and the native menus), `crates/ruthos-core/src/paths.rs`
(folders and stored paths) and `crates/ruthos/src/main.rs` (graphics).

## Archives move between platforms

Relative paths in the archive are stored with forward slashes on every
platform and accepted with either separator when read. Schema version 3
rewrites the backslashes an archive written on Windows before 0.1.0-alpha
holds, so a data folder copied from a PC to a Mac opens with every study,
image, measurement and report, and the reverse holds too. Linked files
(import without copying) keep their absolute paths and do not move.

The macOS data folder is deliberately not in Documents, which iCloud
Drive may sync: a SQLite database in write-ahead-log mode in a synced
folder can be corrupted when the database and its log are uploaded at
different moments. Preferences warns when the chosen folder is inside
iCloud Drive, Dropbox, OneDrive or Google Drive.

## macOS conventions

The native menu bar carries the application menu (About, Preferences with
⌘comma, Hide, Quit with ⌘Q), File, Measurements, Window and Help. The
menus are one definition (`platform::MENU_BAR`) rendered with muda on
macOS; Measurements items go to the viewer window that last had focus.

`Ruthos.app` is built with cargo-packager (`Packager.toml`). Its
Info.plist (`packaging/macos/Info.plist`) sets the bundle identifier
`org.ruthos.Ruthos`, a minimum system version of 11.0, high-resolution
support, the DICOM document type (`.dcm` and `org.nema.dicom`), and the
privacy descriptions macOS requires: local network (the echo test, DICOM
send, teaching sessions), Documents (an existing Horos database), Desktop
and Downloads (imports).

Known gaps to check by hand on a Mac, since no test runner sees them: the
Finder hook adds `application:openURLs:` to winit's application delegate
at runtime and logs whether it succeeded; momentum scrolling on a trackpad
is not yet distinguished from wheel scrolling (egui does not expose the
scroll phase), so a two-finger flick may overshoot a long series.

## Building

Windows: Rust, CMake and NASM (NASM only speeds up libjpeg-turbo). See the
README.

macOS: Rust, the Xcode command-line tools (`xcode-select --install`) and
CMake (`brew install cmake`). Then `cargo build --release`. For the
universal binary build both targets and `lipo` them as `release.yml` does.

Linux: Rust, CMake, NASM and the development packages for GTK 3,
xkbcommon, Wayland and X11 (the apt line in `ci.yml`).

## Continuous integration

`ci.yml` runs on every push and pull request: rustfmt, clippy with
warnings denied and the tests on Linux; the tests on Windows (with NASM)
and on Apple Silicon macOS, which also compiles the Intel slice.

`release.yml` runs on a `v*` tag: a Windows zip (SignPath signing when the
repository variables are set), a universal `Ruthos.app` with a disk image
(Developer ID signing and notarisation when the secrets are set), a Linux
AppImage, and then a draft pre-release with every artefact and
`RELEASE_NOTES.md` as its text. The draft is created only when all three
builds succeed.

## Acceptance

Accepted when the CI matrix is green on all three runners and, by hand on
an Apple Silicon Mac, an Intel Mac and a Windows PC: Metal is used on the
Macs (the log says so); a Finder double-click on a `.dcm` file imports and
opens it; the native menu bar works and tooltips show ⌘ shortcuts; an
archive copied between a PC and a Mac opens on both; the archive is
created under Application Support and an iCloud folder produces the
warning; the firewall prompt appears once for a signed build; the C-ECHO
test produces the Local Network prompt on macOS 15; a trackpad flick does
not overshoot; and exporting a report opens the PDF on all three.
