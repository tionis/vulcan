#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::{parse_document, VaultConfig};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let source = format!("# Links\n\n[[{text}]]\n\n![[{text}]]\n\n[label]({text})\n\n`{text}`\n");
    let _ = parse_document(&source, &VaultConfig::default());
});
