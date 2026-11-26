//! Prepares the Numerals engine (`src/numerals.rs`): the vendored mathjs
//! bundle with a small speed patch, followed by the Numerals prelude, as
//! JavaScript source and as `QuickJS` bytecode. Loading the bytecode skips
//! compiling about 650 KB of JavaScript each time an engine starts.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "js_runtime")]
    numerals::prepare();
}

#[cfg(feature = "js_runtime")]
mod numerals {
    use std::path::PathBuf;

    use rquickjs::{Context, Module, Runtime, WriteOptions, WriteOptionsEndianness};

    const MATHJS: &str = "vendor/mathjs/math.js";
    const PRELUDE: &str = "src/numerals_prelude.js";

    /// Exact replacements in mathjs's bundled typed-function. Creating a
    /// typed function sorts its signatures with a comparator that recounts
    /// each signature's parameters and each parameter's type indexes on
    /// every comparison; for the parser's dependencies that was more than
    /// half of the first `math.evaluate`. These remember the counts on the
    /// signature and parameter objects (built fresh for each typed
    /// function), which leaves the sort order unchanged. A mathjs update
    /// that changes this code fails the build here; see
    /// `vendor/mathjs/README.md`.
    const PATCHES: [(&str, &str); 3] = [
        (
            "function D(e){let t=c.length+1;for(let n=0;n<e.types.length;n++)T(e.types[n])&&(t=Math.min(t,e.types[n].typeIndex));return t}",
            "function D(e){if(void 0!==e.__vulcanD)return e.__vulcanD;let t=c.length+1;for(let n=0;n<e.types.length;n++)T(e.types[n])&&(t=Math.min(t,e.types[n].typeIndex));return e.__vulcanD=t}",
        ),
        (
            "function O(e){let t=l+1;for(let n=0;n<e.types.length;n++)T(e.types[n])||(t=Math.min(t,e.types[n].conversionIndex));return t}",
            "function O(e){if(void 0!==e.__vulcanO)return e.__vulcanO;let t=l+1;for(let n=0;n<e.types.length;n++)T(e.types[n])||(t=Math.min(t,e.types[n].conversionIndex));return e.__vulcanO=t}",
        ),
        (
            "let u,c=0,l=0;for(u of n)u.hasAny&&++c,u.hasConversion&&++l;let f=0,p=0;for(u of r)u.hasAny&&++f,u.hasConversion&&++p;",
            "let u,c=0,l=0,f=0,p=0;if(void 0===n.__vulcanAny){for(u of n)u.hasAny&&++c,u.hasConversion&&++l;n.__vulcanAny=c,n.__vulcanConversions=l}else c=n.__vulcanAny,l=n.__vulcanConversions;if(void 0===r.__vulcanAny){for(u of r)u.hasAny&&++f,u.hasConversion&&++p;r.__vulcanAny=f,r.__vulcanConversions=p}else f=r.__vulcanAny,p=r.__vulcanConversions;",
        ),
    ];

    pub(super) fn prepare() {
        println!("cargo:rerun-if-changed={MATHJS}");
        println!("cargo:rerun-if-changed={PRELUDE}");
        let mut mathjs = std::fs::read_to_string(MATHJS).expect("vendored mathjs should read");
        for (find, replace) in PATCHES {
            assert_eq!(
                mathjs.matches(find).count(),
                1,
                "the mathjs speed patch no longer applies; update PATCHES in vulcan-core/build.rs \
                 for the new bundle (or drop the patch): `{find}`"
            );
            mathjs = mathjs.replacen(find, replace, 1);
        }
        let prelude = std::fs::read_to_string(PRELUDE).expect("Numerals prelude should read");
        // The bundle is a UMD script: called with `this` as the global object
        // it defines `globalThis.math`, also inside a module. Its last line
        // is a source-map comment, hence the line breaks.
        let source = format!(
            "// mathjs 15.2.0 (Apache-2.0), modified by Vulcan: vulcan-core/build.rs memoizes its\n\
             // typed-function signature sort. Followed by vulcan-core/src/numerals_prelude.js.\n\
             (function () {{\n{mathjs}\n}}).call(globalThis);\n{prelude}"
        );

        let endianness = match std::env::var("CARGO_CFG_TARGET_ENDIAN").as_deref() {
            Ok("big") => WriteOptionsEndianness::Big,
            _ => WriteOptionsEndianness::Little,
        };
        let runtime = Runtime::new().expect("QuickJS runtime should start");
        let context = Context::full(&runtime).expect("QuickJS context should start");
        let bytecode = context.with(|ctx| {
            Module::declare(ctx.clone(), "numerals", source.clone())
                .and_then(|module| {
                    module.write(WriteOptions {
                        endianness,
                        ..WriteOptions::default()
                    })
                })
                .unwrap_or_else(|error| {
                    let detail = ctx.catch();
                    panic!("the Numerals engine should compile: {error}: {detail:?}")
                })
        });

        let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
        std::fs::write(out.join("numerals.js"), &source).expect("engine source should write");
        std::fs::write(out.join("numerals.qjsbc"), bytecode).expect("engine bytecode should write");
    }
}
