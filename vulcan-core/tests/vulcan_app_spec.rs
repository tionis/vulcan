use std::fs;
use std::path::PathBuf;

fn spec_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/specs/vulcan-app/v1")
}

#[test]
fn manifest_schema_compiles_and_accepts_the_minimal_fixture() {
    let root = spec_root();
    let schema: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("manifest.schema.json")).expect("read Vulcan App manifest schema"),
    )
    .expect("parse Vulcan App manifest schema");
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("examples/minimal/manifest.source.json"))
            .expect("read minimal Vulcan App manifest"),
    )
    .expect("parse minimal Vulcan App manifest");

    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .expect("Vulcan App manifest schema compiles");
    let errors = validator
        .iter_errors(&manifest)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "minimal manifest is invalid: {errors:#?}"
    );
}

#[test]
fn minimal_fixture_matches_the_normative_identity_vector() {
    let root = spec_root();
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("examples/minimal/manifest.source.json"))
            .expect("read minimal Vulcan App manifest"),
    )
    .expect("parse minimal Vulcan App manifest");
    let vector: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("examples/minimal/identity-test-vector.json"))
            .expect("read Vulcan App identity vector"),
    )
    .expect("parse Vulcan App identity vector");

    let canonical = serde_json_canonicalizer::to_vec(&manifest)
        .expect("minimal manifest has a canonical RFC 8785 representation");
    assert_eq!(
        canonical.len() as u64,
        vector["canonical_manifest_bytes"]
            .as_u64()
            .expect("canonical byte length")
    );
    assert_eq!(
        derive_id("dev.vulcan.app-content.v1", &canonical),
        vector["app_content_id"]
            .as_str()
            .expect("AppContentId vector")
    );

    let payload = fs::read(root.join("examples/minimal/assets/index.html"))
        .expect("read minimal Vulcan App payload");
    let payload_vector = &vector["payloads"]["assets/index.html"];
    assert_eq!(
        payload.len() as u64,
        payload_vector["bytes"].as_u64().expect("payload bytes")
    );
    assert_eq!(
        derive_id("dev.vulcan.app-payload.v1", &payload),
        payload_vector["digest"].as_str().expect("payload digest")
    );
}

#[test]
fn server_component_contract_has_no_ambient_wasi_imports() {
    let wit = fs::read_to_string(spec_root().join("vulcan-app.wit"))
        .expect("read Vulcan App WIT contract");
    assert!(wit.contains("import host;"));
    assert!(wit.contains("export invoke:"));
    assert!(!wit.contains("wasi:"));
}

fn derive_id(context: &str, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    hasher.update(bytes);
    format!("blake3:{}", hasher.finalize().to_hex())
}
