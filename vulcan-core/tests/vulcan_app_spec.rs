use std::fs;
use std::io::Read;
use std::path::PathBuf;

use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{json, Value};

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

fn fixture_json(path: &str) -> Value {
    serde_json::from_slice(&fs::read(spec_root().join(path)).expect("read JSON fixture"))
        .expect("parse JSON fixture")
}

fn manifest_validator() -> jsonschema::Validator {
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&fixture_json("manifest.schema.json"))
        .expect("compile manifest schema")
}

#[test]
fn store_profiles_reject_conflicting_authority_replication_and_retention() {
    let validator = manifest_validator();
    let base = fixture_json("examples/minimal/manifest.source.json");
    let profiles = [
        (
            "temporary",
            vec!["kv", "blob"],
            "temporary",
            "invocation",
            "none",
            "private",
            "disposable",
        ),
        (
            "derived-cache",
            vec!["kv", "sqlite", "blob"],
            "derived",
            "device",
            "none",
            "admin",
            "disposable",
        ),
        (
            "device-local",
            vec!["kv", "sqlite"],
            "authoritative",
            "device",
            "none",
            "admin",
            "preserve",
        ),
        (
            "vault-collection",
            vec!["mdbase"],
            "authoritative",
            "vault",
            "file_tree",
            "vault",
            "preserve",
        ),
        (
            "canonical-artifact",
            vec!["canonical_artifact", "blob"],
            "authoritative",
            "vault",
            "file_tree",
            "vault",
            "preserve",
        ),
        (
            "live-session",
            vec!["live_session"],
            "temporary",
            "group",
            "external",
            "admin",
            "disposable",
        ),
    ];
    for (profile, engines, authority, scope, replication, visibility, retention) in profiles {
        for engine in engines {
            let mut manifest = base.clone();
            manifest["stores"]["data"] = json!({
                "profile": profile, "engine": engine, "authority": authority,
                "scope": scope, "replication": replication, "visibility": visibility,
                "retention": retention, "schema_version": 1, "quota_bytes": 1_048_576
            });
            assert!(validator.is_valid(&manifest), "valid {profile}/{engine}");
            assert_store_field_rejected(
                &validator,
                &manifest,
                "authority",
                authority,
                "authoritative",
                "derived",
            );
            assert_store_field_rejected(
                &validator,
                &manifest,
                "replication",
                replication,
                "none",
                "file_tree",
            );
            assert_store_field_rejected(
                &validator,
                &manifest,
                "retention",
                retention,
                "preserve",
                "disposable",
            );
        }
    }
}

fn assert_store_field_rejected(
    validator: &jsonschema::Validator,
    manifest: &Value,
    field: &str,
    current: &str,
    first: &str,
    second: &str,
) {
    let mut invalid = manifest.clone();
    invalid["stores"]["data"][field] = json!(if current == first { second } else { first });
    assert!(
        !validator.is_valid(&invalid),
        "accepted inconsistent store {field}"
    );
}

#[test]
fn capabilities_accept_resource_wildcards_but_reject_wrong_dimensions() {
    let validator = manifest_validator();
    let mut manifest = fixture_json("examples/minimal/manifest.source.json");
    for (name, selector) in [
        ("app.stores.read", json!({"stores": ["*"]})),
        ("app.functions.invoke", json!({"functions": ["*"]})),
        (
            "secrets.use",
            json!({"secrets": ["*"], "domains": ["example.com"], "operations": ["fetch"]}),
        ),
        (
            "processors.use",
            json!({"processors": ["converter"], "operations": ["submit"]}),
        ),
        ("runtime.clock", json!({})),
        ("runtime.random", json!({})),
    ] {
        manifest["capability_requests"] =
            json!([{"id":"request", "name":name, "required":true, "selector":selector}]);
        assert!(validator.is_valid(&manifest), "valid capability {name}");
        manifest["capability_requests"][0]["selector"] = json!({"unsupported": ["*"]});
        assert!(!validator.is_valid(&manifest), "unknown dimension {name}");
    }
    manifest["capability_requests"] = json!([{"id":"request", "name":"app.stores.read", "required":true, "selector":{"paths":["*"]}}]);
    assert!(
        !validator.is_valid(&manifest),
        "paths cannot grant store access"
    );
    manifest["capability_requests"] = json!([]);
    manifest["entrypoints"]["views"]["main"]["capability_requests"] = json!(["*"]);
    assert!(
        !validator.is_valid(&manifest),
        "declaration references are not selectors"
    );
}

#[test]
fn manifest_can_declare_migrations_bindings_and_entrypoint_requirements() {
    let validator = manifest_validator();
    let mut manifest = fixture_json("examples/signed-store/manifest.source.json");
    assert!(validator.is_valid(&manifest), "signed SQL store fixture");
    manifest["configuration"] = json!({"version":1, "schema":"schemas/config.json"});
    manifest["stores"]["records"] = json!({
        "profile":"vault-collection", "engine":"mdbase", "authority":"authoritative",
        "scope":"vault", "replication":"file_tree", "visibility":"vault", "retention":"preserve",
        "schema_version":1, "quota_bytes":1_048_576, "binding_required":true
    });
    manifest["entrypoints"]["views"]["main"]["store_requirements"] = json!({
        "records":["core_read", "vulcan.record_write.v1", "vulcan.saved_views.v1"]
    });
    // Shape checks deliberately do not claim to resolve package cross-references.
    assert!(validator.is_valid(&manifest));
    manifest["stores"]["records"]["migrations"] = json!([]);
    assert!(
        !validator.is_valid(&manifest),
        "SQL migration field on an MDB store"
    );
    manifest["stores"]["records"]
        .as_object_mut()
        .unwrap()
        .remove("migrations");
    manifest["stores"]["records"]["adapter"] = json!("sqlite-artifact-v1");
    assert!(
        !validator.is_valid(&manifest),
        "SQL artifact adapter on an MDB store"
    );
    manifest["stores"]["records"]
        .as_object_mut()
        .unwrap()
        .remove("adapter");
    manifest["stores"]["records"]["command_function"] = json!("reduce");
    assert!(
        !validator.is_valid(&manifest),
        "live reducer on an MDB store"
    );
}

#[test]
fn app_ids_require_a_dot_and_agree_with_the_documented_length_limit() {
    let validator = manifest_validator();
    let mut manifest = fixture_json("examples/minimal/manifest.source.json");
    for id in ["single", "two-parts", "a..b", "a_b.c", "Upper.case"] {
        manifest["app"]["id"] = json!(id);
        assert!(!validator.is_valid(&manifest), "invalid app ID {id}");
    }
    manifest["app"]["id"] = json!(format!("a.{}", "b".repeat(94)));
    assert!(validator.is_valid(&manifest));
    manifest["app"]["id"] = json!(format!("a.{}", "b".repeat(95)));
    assert!(!validator.is_valid(&manifest));
}

fn decode_hex<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).expect("hex fixture"))
}

#[test]
fn signed_fixture_binds_manifest_policy_payloads_and_exact_archive() {
    let root = spec_root().join("examples/signed-store");
    let manifest = fixture_json("examples/signed-store/manifest.source.json");
    let vector = fixture_json("examples/signed-store/identity-test-vector.json");
    let signature = fixture_json("examples/signed-store/META-INF/signatures/test-key.json");
    let validator = jsonschema::draft202012::options()
        .build(&fixture_json("signature.schema.json"))
        .expect("signature schema");
    assert!(validator.is_valid(&signature));
    let mut invalid = signature.clone();
    invalid["algorithm"] = json!("none");
    assert!(!validator.is_valid(&invalid));
    invalid = signature.clone();
    invalid["signature"] = json!("00");
    assert!(!validator.is_valid(&invalid));
    invalid = signature.clone();
    invalid["unknown"] = json!(true);
    assert!(!validator.is_valid(&invalid));

    let canonical = serde_json_canonicalizer::to_vec(&manifest).expect("canonical manifest");
    let content_id = derive_id("dev.vulcan.app-content.v1", &canonical);
    assert_eq!(vector["app_content_id"], content_id);
    assert_eq!(vector["canonical_manifest_bytes"], canonical.len());
    assert_eq!(signature["app_content_id"], content_id);
    let mut statement = b"vulcan-app-signature/v1\0".to_vec();
    statement.extend(decode_hex::<32>(&content_id[7..]));
    assert_eq!(
        statement,
        decode_hex::<{ b"vulcan-app-signature/v1\0".len() + 32 }>(
            vector["signed_statement_hex"].as_str().unwrap()
        )
    );
    let public_key =
        VerifyingKey::from_bytes(&decode_hex(signature["public_key"].as_str().unwrap()))
            .expect("valid public test key");
    let signature_bytes =
        Signature::from_bytes(&decode_hex(signature["signature"].as_str().unwrap()));
    public_key
        .verify_strict(&statement, &signature_bytes)
        .expect("valid signature");
    statement[0] ^= 1;
    assert!(public_key
        .verify_strict(&statement, &signature_bytes)
        .is_err());
    for field in ["publisher", "key_id", "public_key"] {
        assert_eq!(
            manifest["signature_policy"]["required_signers"][0][field],
            signature[field]
        );
    }
    let mut stripped_policy = manifest.clone();
    stripped_policy
        .as_object_mut()
        .unwrap()
        .remove("signature_policy");
    assert_ne!(
        content_id,
        derive_id(
            "dev.vulcan.app-content.v1",
            &serde_json_canonicalizer::to_vec(&stripped_policy).unwrap()
        )
    );

    let blob = fs::read(root.join("package.vapp")).unwrap();
    assert_eq!(
        vector["package_blob_id"],
        derive_id("dev.vulcan.app-package-blob.v1", &blob)
    );
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(blob)).expect("fixture ZIP");
    let mut names = manifest["files"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    names.push("META-INF/signatures/test-key.json".to_owned());
    names.sort();
    names.insert(0, "manifest.json".to_owned());
    assert_eq!(archive.len(), names.len());
    for (index, name) in names.iter().enumerate() {
        let mut entry = archive.by_index(index).unwrap();
        assert_eq!(entry.name(), name);
        assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if name == "manifest.json" {
            assert_eq!(bytes, canonical);
        } else {
            assert_eq!(bytes, fs::read(root.join(name)).unwrap());
            if let Some(payload) = manifest["files"].get(name) {
                assert_eq!(payload["size"], bytes.len());
                assert_eq!(
                    payload["digest"],
                    derive_id("dev.vulcan.app-payload.v1", &bytes)
                );
            } else {
                assert_eq!(bytes, serde_json_canonicalizer::to_vec(&signature).unwrap());
            }
        }
    }
}

#[test]
fn declared_sql_initialization_creates_the_fixture_store() {
    let manifest = fixture_json("examples/signed-store/manifest.source.json");
    let migration = &manifest["stores"]["settings"]["migrations"][0];
    assert_eq!(migration["from_version"], 0);
    assert_eq!(
        migration["to_version"],
        manifest["stores"]["settings"]["schema_version"]
    );
    let sql = fs::read_to_string(
        spec_root()
            .join("examples/signed-store")
            .join(migration["path"].as_str().unwrap()),
    )
    .unwrap();
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    transaction.execute_batch(&sql).expect("initialization SQL");
    transaction
        .execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)",
            ["theme", "dark"],
        )
        .unwrap();
    transaction.commit().unwrap();
    let value: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            ["theme"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "dark");
}
