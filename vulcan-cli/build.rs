use std::{env, path::Path, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=windows/vulcan.manifest");
    stamp_local_build_version();

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Vulcan's Windows console-allocation manifest requires the supported MSVC target"
    );

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("windows")
        .join("vulcan.manifest");
    println!("cargo:rustc-link-arg-bin=vulcan=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=vulcan=/MANIFESTINPUT:{}",
        manifest.display()
    );
}

/// A build from a Git checkout without `VULCAN_BUILD_VERSION` (set by the
/// release workflows) gets a dev version shaped like the rolling release's,
/// `<major>.<minor>.<patch+1>-dev.<commit date>.0.g<commit>`. It sorts above
/// published builds from earlier days and below any CI build of the same
/// day, so `self-update` does not "update" a newer local build to an older
/// published one. Outside this repository's checkout (a source tarball) the
/// package version stays as it is.
fn stamp_local_build_version() {
    println!("cargo:rerun-if-env-changed=VULCAN_BUILD_VERSION");
    if env::var_os("VULCAN_BUILD_VERSION").is_some() {
        return;
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let Some(workspace) = manifest_dir.parent() else {
        return;
    };
    let git = |arguments: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(arguments)
            .env("TZ", "UTC")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|text| text.trim().to_string())
    };
    let Some(toplevel) = git(&["rev-parse", "--show-toplevel"]) else {
        return;
    };
    let same_dir = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same_dir(Path::new(&toplevel), workspace) {
        return;
    }
    let (Some(commit), Some(date)) = (
        git(&["rev-parse", "HEAD"]),
        git(&["log", "-1", "--format=%cd", "--date=format-local:%Y%m%d"]),
    ) else {
        return;
    };
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts = version.split('.').map(str::parse::<u64>);
    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return;
    };
    let short = &commit[..commit.len().min(8)];
    println!(
        "cargo:rustc-env=VULCAN_BUILD_VERSION={major}.{minor}.{}-dev.{date}.0.g{short}",
        patch + 1
    );
    // Rebuild when HEAD moves to another commit.
    if let Some(head) = git(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &reference,
        ]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(packed) = git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        "packed-refs",
    ]) {
        println!("cargo:rerun-if-changed={packed}");
    }
}
