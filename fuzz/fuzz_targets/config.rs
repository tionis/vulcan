#![no_main]

use libfuzzer_sys::fuzz_target;
use vulcan_core::validate_vulcan_overrides_toml;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = validate_vulcan_overrides_toml(source.as_ref());
});
