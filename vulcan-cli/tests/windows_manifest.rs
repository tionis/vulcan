const WINDOWS_MANIFEST: &str = include_str!("../windows/vulcan.manifest");

#[test]
fn windows_manifest_requests_detached_console_allocation() {
    assert!(WINDOWS_MANIFEST.contains(
        "<consoleAllocationPolicy xmlns=\"http://schemas.microsoft.com/SMI/2024/WindowsSettings\">detached</consoleAllocationPolicy>"
    ));
}

#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn windows_binary_embeds_detached_console_allocation_manifest() {
    use std::{fs, process::Command};

    let extracted = tempfile::NamedTempFile::new().unwrap();
    let input = format!("-inputresource:{};#1", env!("CARGO_BIN_EXE_vulcan"));
    let output = format!("-out:{}", extracted.path().display());
    let result = Command::new("mt.exe")
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
