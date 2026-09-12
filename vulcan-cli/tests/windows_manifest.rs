const WINDOWS_MANIFEST: &str = include_str!("../windows/vulcan.manifest");

use std::{
    fs,
    path::{Path, PathBuf},
};

fn find_manifest_tool_in(kits_bin: &Path) -> Option<PathBuf> {
    let mut versions = fs::read_dir(kits_bin)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    versions.sort_by_key(fs::DirEntry::file_name);
    versions.into_iter().rev().find_map(|entry| {
        let candidate = entry.path().join("x64").join("mt.exe");
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(windows)]
fn find_manifest_tool() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join("mt.exe"))
        .find(|path| path.is_file())
        .or_else(|| {
            let program_files = std::env::var_os("ProgramFiles(x86)")?;
            find_manifest_tool_in(
                &PathBuf::from(program_files)
                    .join("Windows Kits")
                    .join("10")
                    .join("bin"),
            )
        })
}

#[test]
fn windows_manifest_requests_detached_console_allocation() {
    assert!(WINDOWS_MANIFEST.contains(
        "<consoleAllocationPolicy xmlns=\"http://schemas.microsoft.com/SMI/2024/WindowsSettings\">detached</consoleAllocationPolicy>"
    ));
}

#[test]
fn manifest_tool_discovery_selects_the_newest_installed_windows_sdk() {
    let directory = tempfile::tempdir().unwrap();
    for version in ["10.0.22621.0", "10.0.26100.0"] {
        let tool = directory.path().join(version).join("x64").join("mt.exe");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(tool, []).unwrap();
    }

    assert_eq!(
        find_manifest_tool_in(directory.path()).unwrap(),
        directory
            .path()
            .join("10.0.26100.0")
            .join("x64")
            .join("mt.exe")
    );
}

#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn windows_binary_embeds_detached_console_allocation_manifest() {
    use std::process::Command;

    let extracted = tempfile::NamedTempFile::new().unwrap();
    let input = format!("-inputresource:{};#1", env!("CARGO_BIN_EXE_vulcan"));
    let output = format!("-out:{}", extracted.path().display());
    let tool =
        find_manifest_tool().expect("mt.exe should be available from PATH or the Windows SDK");
    let result = Command::new(tool)
        .args(["-nologo", &input, &output])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "mt.exe failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let manifest = fs::read_to_string(extracted.path()).unwrap();
    assert!(manifest.contains(
        "<consoleAllocationPolicy xmlns=\"http://schemas.microsoft.com/SMI/2024/WindowsSettings\">detached</consoleAllocationPolicy>"
    ));
}
