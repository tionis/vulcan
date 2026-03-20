#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::{parse_document, VaultConfig};

fuzz_target!(|data: &[u8]| {
    let yaml = std::str::from_utf8(data).unwrap_or("");
    let source = format!("---\n{yaml}\n---\nbody\n");
    let _ = parse_document(&source, &VaultConfig::default());
});
