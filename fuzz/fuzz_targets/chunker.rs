#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::{parse_document, ChunkingStrategy, VaultConfig};

fuzz_target!(|data: &[u8]| {
    let source = std::str::from_utf8(data).unwrap_or("");
    for strategy in [
        ChunkingStrategy::Heading,
        ChunkingStrategy::Fixed,
        ChunkingStrategy::Paragraph,
    ] {
        let mut config = VaultConfig::default();
        config.chunking.strategy = strategy;
        config.chunking.target_size = 64;
        config.chunking.overlap = 8;
        let _ = parse_document(source, &config);
    }
});
