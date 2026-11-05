//! Records the commit a build came from, for the About window.

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let commit = std::env::var("GITHUB_SHA").ok().or_else(|| {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    let short: String = commit
        .unwrap_or_else(|| "unknown".to_string())
        .chars()
        .take(7)
        .collect();
    println!("cargo:rustc-env=RUTHOS_COMMIT={short}");
}
