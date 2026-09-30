//! Stamps the commit the binary is built from into `CLANKJOB_COMMIT`, which `/healthz`
//! reports and the web UI shows. The Docker build has no `.git` and passes the commit in
//! the `CLANKJOB_COMMIT` build argument instead; without either it is "unknown".

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CLANKJOB_COMMIT");
    let commit = std::env::var("CLANKJOB_COMMIT")
        .ok()
        .filter(|commit| !commit.trim().is_empty())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_owned());
    let short: String = commit.trim().chars().take(7).collect();
    println!("cargo:rustc-env=CLANKJOB_COMMIT={short}");
}

/// The checkout's current commit, and rerun the build script when it moves.
fn git_commit() -> Option<String> {
    let output = Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // A new commit rewrites the branch's ref (or packed-refs); a checkout rewrites HEAD.
    for path in ["../../.git/HEAD", "../../.git/refs/heads", "../../.git/packed-refs"] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    String::from_utf8(output.stdout).ok()
}
