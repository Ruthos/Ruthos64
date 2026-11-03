//! Embeds the Windows version resource in ruthos.exe: product name,
//! description, version and original file name, which the SignPath
//! Foundation requires on every binary it signs and which Explorer shows
//! under Properties > Details. Nothing happens on other platforms.

fn main() {
    #[cfg(windows)]
    {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        // FileVersion is numeric (major.minor.patch.0); ProductVersion
        // keeps the full text, such as 0.1.0-alpha.
        let numeric: Vec<u64> = version
            .split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let packed = numeric.iter().fold(0u64, |acc, v| (acc << 16) | v) << 16;
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Ruthos")
            .set("FileDescription", "Ruthos DICOM viewer and patient archive")
            .set("CompanyName", "The Ruthos contributors")
            .set("LegalCopyright", "The Ruthos contributors, LGPL-3.0-only")
            .set("OriginalFilename", "ruthos.exe")
            .set("InternalName", "ruthos")
            .set("ProductVersion", &version)
            .set("FileVersion", &version)
            .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
            .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        if let Err(e) = res.compile() {
            // A missing resource compiler must not stop a developer build;
            // the release workflow checks the result.
            println!("cargo:warning=version resource not embedded: {e}");
        }
    }
}
