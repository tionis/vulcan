use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=windows/vulcan.manifest");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    assert_eq!(
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        Ok("msvc"),
        "Vulcan's Windows console-allocation manifest requires the supported MSVC target"
    );

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("windows")
        .join("vulcan.manifest");
    println!("cargo:rustc-link-arg-bin=vulcan=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=vulcan=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
