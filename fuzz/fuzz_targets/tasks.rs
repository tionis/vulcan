#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::parse_tasks_query;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = parse_tasks_query(source.as_ref());
});
