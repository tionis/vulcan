#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::{parse_document, VaultConfig};

fuzz_target!(|data: &[u8]| {
    let text = std::str::from_utf8(data).unwrap_or("");
    let source = format!(
        "# Links\n\n[[{text}]]\n\n![[{text}]]\n\n[label]({text})\n\n`{text}`\n"
    );
    let _ = parse_document(&source, &VaultConfig::default());
});
