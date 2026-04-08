#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::expression::parse_expression;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = parse_expression(source.as_ref());
});
