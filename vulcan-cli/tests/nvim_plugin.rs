//! Runs the vulcan.nvim headless smoke test against the built binary.
//!
//! Skipped when `nvim` 0.10 or newer is not on `PATH`, unless
//! `VULCAN_REQUIRE_NVIM` is set, which turns a missing Neovim into a failure.

use std::path::Path;
use std::process::Command;

fn neovim_available() -> bool {
    let Ok(output) = Command::new("nvim")
        .args([
            "--headless",
            "-u",
            "NONE",
            "--clean",
            "+lua io.write(vim.fn.has('nvim-0.10'))",
            "+qa!",
        ])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout).trim() == "1"
        || String::from_utf8_lossy(&output.stderr).trim() == "1"
}

#[test]
fn vulcan_nvim_smoke_test_passes() {
    if !neovim_available() {
        assert!(
            std::env::var_os("VULCAN_REQUIRE_NVIM").is_none(),
            "VULCAN_REQUIRE_NVIM is set but nvim 0.10+ is not available"
        );
        eprintln!("skipping: nvim 0.10+ is not on PATH");
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("integrations/vulcan.nvim/test/smoke.lua");
    // Keep the user's Neovim state and logs out of the test.
    let state = tempfile::tempdir().expect("state dir");
    let output = Command::new("nvim")
        .args(["--headless", "-u", "NONE", "-l"])
        .arg(&script)
        .arg(env!("CARGO_BIN_EXE_vulcan"))
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("nvim should run");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && report.contains("all checks passed"),
        "vulcan.nvim smoke test failed:\n{report}"
    );
}
