#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::dql::{parse_dql, parse_dql_with_diagnostics};

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = parse_dql(source.as_ref());
    let _ = parse_dql_with_diagnostics(source.as_ref());
});
