//! Runs the vulcan.vim headless smoke test against the built binary.
//!
//! Needs Vim 9.0.0067 or newer on `PATH` and a yegappan/lsp checkout named by
//! `VULCAN_VIM_LSP`. Skipped without them, unless `VULCAN_REQUIRE_VIM` is
//! set, which turns a missing Vim or yegappan/lsp into a failure.

use std::path::{Path, PathBuf};
use std::process::Command;

fn vim_available() -> bool {
    let Ok(output) = Command::new("vim")
        .args(["-Nu", "NONE", "-i", "NONE", "-es"])
        .args(["+if has('patch-9.0.0067') | qall! | else | cquit | endif"])
        .output()
    else {
        return false;
    };
    output.status.success()
}

fn yegappan_lsp() -> Option<PathBuf> {
    std::env::var_os("VULCAN_VIM_LSP")
        .map(PathBuf::from)
        .filter(|path| path.join("plugin/lsp.vim").is_file())
}

#[test]
fn vulcan_vim_smoke_test_passes() {
    let (true, Some(lsp)) = (vim_available(), yegappan_lsp()) else {
        assert!(
            std::env::var_os("VULCAN_REQUIRE_VIM").is_none(),
            "VULCAN_REQUIRE_VIM is set but Vim 9 or VULCAN_VIM_LSP (a yegappan/lsp checkout) is missing"
        );
        eprintln!("skipping: needs Vim 9 on PATH and VULCAN_VIM_LSP pointing at yegappan/lsp");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("integrations/vulcan.vim/test/smoke.vim");
    let state = tempfile::tempdir().expect("state dir");
    let log = state.path().join("smoke.log");
    let output = Command::new("vim")
        .args(["-Nu", "NONE", "-i", "NONE", "-es", "-S"])
        .arg(&script)
        .env("VULCAN_BIN", env!("CARGO_BIN_EXE_vulcan"))
        .env("VULCAN_VIM_LSP", &lsp)
        .env("VULCAN_SMOKE_LOG", &log)
        .env("HOME", state.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("vim should run");
    let report = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        output.status.success() && report.contains("all checks passed"),
        "vulcan.vim smoke test failed:\n{report}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
