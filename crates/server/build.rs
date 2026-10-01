//! Stamps the version and the commit the binary is built from into `CLANKJOB_VERSION` and
//! `CLANKJOB_COMMIT`, which `/healthz` reports and the web UI shows.
//!
//! The version is the release tag when one is given (`CLANKJOB_VERSION`, set by the image
//! workflow from a `v1.2.3` tag), else `git describe` of the checkout (`1.2.3` on a tag,
//! `1.2.3-4-gabc1234` four commits after it, `-dirty` with local changes), else the crate's
//! version marked `-dev`. The Docker build has no `.git`: it gets the commit in the
//! `CLANKJOB_COMMIT` build argument instead; without either it is "unknown".

use std::path::Path;
use std::process::Command;

/// A build environment variable, when set and not blank.
fn given(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Run git in the checkout, rerunning the build script when the checkout moves.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // A new commit rewrites the branch's ref (or packed-refs), a checkout rewrites HEAD, and
    // a new tag adds a ref under refs/tags.
    for path in [
        "../../.git/HEAD",
        "../../.git/refs/heads",
        "../../.git/refs/tags",
        "../../.git/packed-refs",
    ] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    String::from_utf8(output.stdout).ok().map(|text| text.trim().to_owned())
}

fn main() {
    let commit = given("CLANKJOB_COMMIT")
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    let short: String = commit.chars().take(7).collect();
    println!("cargo:rustc-env=CLANKJOB_COMMIT={short}");

    let version = given("CLANKJOB_VERSION")
        .or_else(|| git(&["describe", "--tags", "--match", "v[0-9]*", "--dirty"]))
        .map_or_else(
            || format!("{}-dev", env!("CARGO_PKG_VERSION")),
            |version| version.trim_start_matches('v').to_owned(),
        );
    println!("cargo:rustc-env=CLANKJOB_VERSION={version}");
}
