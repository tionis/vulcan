//! Process-wide validators for immutable, pinned control-file envelopes only.
//! User-authored schemas remain collection-scoped and permission checked.
use super::{
    bundled_mdbase_schema, schema_diagnostics, MdbaseSchemaCompileError, MdbaseSchemaDiagnostic,
    MdbaseSchemaRetriever, MDBASE_BUNDLED_SCHEMAS, MDBASE_CANONICAL_SCHEMA_BASE,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy)]
pub(super) enum EnvelopeSchema {
    Type,
    Contract,
}

static TYPE: LazyLock<Result<jsonschema::Validator, MdbaseSchemaCompileError>> =
    LazyLock::new(|| compile(EnvelopeSchema::Type));
static CONTRACT: LazyLock<Result<jsonschema::Validator, MdbaseSchemaCompileError>> =
    LazyLock::new(|| compile(EnvelopeSchema::Contract));

#[cfg(test)]
static TYPE_COMPILATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static CONTRACT_COMPILATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl EnvelopeSchema {
    fn name(self) -> &'static str {
        match self {
            Self::Type => "type-file.schema.json",
            Self::Contract => "data-contract.schema.json",
        }
    }

    fn validator(self) -> Result<&'static jsonschema::Validator, MdbaseSchemaCompileError> {
        match self {
            Self::Type => &*TYPE,
            Self::Contract => &*CONTRACT,
        }
        .as_ref()
        .map_err(Clone::clone)
    }

    pub(super) fn validate(
        self,
        value: &Value,
    ) -> Result<Vec<MdbaseSchemaDiagnostic>, MdbaseSchemaCompileError> {
        Ok(schema_diagnostics(self.validator()?, value))
    }
}

fn compile(kind: EnvelopeSchema) -> Result<jsonschema::Validator, MdbaseSchemaCompileError> {
    #[cfg(test)]
    match kind {
        EnvelopeSchema::Type => &TYPE_COMPILATIONS,
        EnvelopeSchema::Contract => &CONTRACT_COMPILATIONS,
    }
    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let canonical = format!("{MDBASE_CANONICAL_SCHEMA_BASE}{}", kind.name());
    let bundled = bundled_mdbase_schema(&canonical).expect("control envelope is bundled");
    let schema: Value = serde_json::from_str(bundled.json)
        .map_err(|error| MdbaseSchemaCompileError(error.to_string()))?;
    let schemas = MDBASE_BUNDLED_SCHEMAS
        .iter()
        .map(|bundled| {
            serde_json::from_str(bundled.json)
                .map(|value| (bundled.canonical_id.to_string(), value))
        })
        .collect::<Result<HashMap<String, Value>, _>>()
        .map_err(|error| MdbaseSchemaCompileError(error.to_string()))?;
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .with_base_uri(canonical)
        .with_retriever(MdbaseSchemaRetriever { schemas })
        .build(&schema)
        .map_err(|error| MdbaseSchemaCompileError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[test]
    #[ignore = "component microbenchmark; run release, serialized, with --nocapture"]
    fn envelope_schema_component_benchmark() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("control.md");
        std::fs::write(&base, "---\n---\n").unwrap();
        for kind in [EnvelopeSchema::Type, EnvelopeSchema::Contract] {
            let canonical = format!("{MDBASE_CANONICAL_SCHEMA_BASE}{}", kind.name());
            let schema: Value =
                serde_json::from_str(bundled_mdbase_schema(&canonical).unwrap().json).unwrap();
            let values = (0..32).map(|index| match kind {
                EnvelopeSchema::Type => json!({"kind":"mdbase.type", "name":format!("task_{index}"), "schema":{"dialect":"json-schema-2020-12", "value":{"type":"object"}}}),
                EnvelopeSchema::Contract => json!({"kind":"mdbase.contract", "contract_type":"record", "id":format!("example.note_{index}"), "version":"1.0.0", "record_schema":{"dialect":"json-schema-2020-12", "value":{"type":"object"}}}),
            }).collect::<Vec<_>>();
            for retained in [false, true] {
                let validate = |value: &Value| {
                    if retained {
                        kind.validate(value)
                    } else {
                        super::super::validate_mdbase_schema_value_with_local_refs(
                            &schema,
                            value,
                            &base,
                            directory.path(),
                        )
                    }
                    .unwrap()
                };
                for value in values.iter().cycle().take(20) {
                    assert!(validate(value).is_empty());
                }
                let mut micros = Vec::with_capacity(1000);
                for value in values.iter().cycle().take(1000) {
                    let start = std::time::Instant::now();
                    let diagnostics = validate(std::hint::black_box(value));
                    micros.push(start.elapsed().as_secs_f64() * 1_000_000.0);
                    assert!(diagnostics.is_empty());
                }
                micros.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    json!({
                        "component":"mdbase_control_envelope", "schema":kind.name(),
                        "mode":if retained {"retained"} else {"legacy_compile_per_file"},
                        "optimized":!cfg!(debug_assertions), "warmup":20, "samples":micros.len(), "errors":0,
                        "p50_us":micros[499], "p95_us":micros[949], "p99_us":micros[989],
                    })
                );
            }
        }
    }

    #[test]
    fn control_envelopes_compile_once_under_concurrent_repeated_validation() {
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..128 {
                        for kind in [EnvelopeSchema::Type, EnvelopeSchema::Contract] {
                            assert!(!kind.validate(&json!({})).unwrap().is_empty());
                            assert!(std::ptr::eq(
                                kind.validator().unwrap(),
                                kind.validator().unwrap()
                            ));
                        }
                    }
                });
            }
        });
        assert_eq!(TYPE_COMPILATIONS.load(Ordering::Relaxed), 1);
        assert_eq!(CONTRACT_COMPILATIONS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn shared_envelopes_preserve_local_compiler_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("control.md");
        std::fs::write(&base, "---\n---\n").unwrap();
        let values = [
            json!({}),
            json!({"kind": 1}),
            json!({"kind":"mdbase.type", "name":"task", "schema":{"dialect":"json-schema-2020-12", "value":{"type":"object"}}}),
            json!({"kind":"mdbase.contract", "contract_type":"record", "id":"example.note", "version":"1.0.0", "record_schema":{"dialect":"json-schema-2020-12", "value":{"type":"object"}}}),
        ];
        for kind in [EnvelopeSchema::Type, EnvelopeSchema::Contract] {
            let canonical = format!("{MDBASE_CANONICAL_SCHEMA_BASE}{}", kind.name());
            let schema: Value =
                serde_json::from_str(bundled_mdbase_schema(&canonical).unwrap().json).unwrap();
            for value in &values {
                let original = super::super::validate_mdbase_schema_value_with_local_refs(
                    &schema,
                    value,
                    &base,
                    directory.path(),
                )
                .unwrap();
                assert_eq!(kind.validate(value).unwrap(), original);
            }
        }
        // Validation of these fixed schemas has no collection dependency.
        drop(directory);
        assert!(!EnvelopeSchema::Type
            .validate(&json!({}))
            .unwrap()
            .is_empty());
    }
}
