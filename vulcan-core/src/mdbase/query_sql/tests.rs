use crate::mdbase::MdbaseCelEngine;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn sql_predicates_do_not_prove_cel_input_limits() {
    use crate::mdbase::MdbaseCelLimits;
    let engine = MdbaseCelEngine::new(MdbaseCelLimits {
        max_value_bytes: 128,
        ..MdbaseCelLimits::default()
    });
    let program = engine.compile("status == 'open'").unwrap();
    let fields = json!({"status": "closed", "unrelated": "x".repeat(1024)});
    let bindings = fields
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert!(!selected("status == 'open'", &fields, "a.md"));
    assert!(engine
        .evaluate(&program, &bindings)
        .unwrap_err()
        .message
        .contains("input value size"));
}

#[test]
fn prepared_sql_predicates_reuse_canonical_programs_without_eager_lazy_compilation() {
    use crate::mdbase::{compile_mdbase_prepared_query, MdbasePreparedQuery};
    let prepared = compile_mdbase_prepared_query(&json!({"where": "status == 'open'"})).unwrap();
    let predicate = prepared.sql_filter_predicate().unwrap();
    assert!(std::ptr::eq(
        predicate,
        prepared.sql_filter_predicate().unwrap()
    ));
    let lazy = MdbasePreparedQuery::new(prepared.plan());
    assert!(lazy.sql_filter_predicate().is_none());
    let mut malformed = prepared.plan().clone();
    malformed.filter.as_mut().unwrap().source = "broken(".into();
    assert!(MdbasePreparedQuery::new(&malformed)
        .sql_filter_predicate()
        .is_none());
    for query in [
        json!({"where": "status == 'open'", "projections": {"x": {"expr": "1/0"}}}),
        json!({"where": "status == 'open'", "context": {"this": {"path": "other.md"}}}),
    ] {
        assert!(compile_mdbase_prepared_query(&query)
            .unwrap()
            .sql_filter_predicate()
            .is_none());
    }
}

fn selected(expression: &str, fields: &serde_json::Value, path: &str) -> bool {
    let engine = MdbaseCelEngine::default();
    let program = engine.compile(expression).unwrap();
    let predicate = program.sql_predicate().expect("supported scalar filter");
    let database = rusqlite::Connection::open_in_memory().unwrap();
    database
        .execute_batch("CREATE TABLE record(path TEXT, effective_frontmatter_json TEXT)")
        .unwrap();
    database
        .execute(
            "INSERT INTO record VALUES (?1, ?2)",
            rusqlite::params![path, fields.to_string()],
        )
        .unwrap();
    let mut parameters = Vec::new();
    let clause = predicate.render(&mut parameters);
    database
        .query_row(
            &format!("SELECT {clause} FROM record"),
            rusqlite::params_from_iter(parameters),
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn sql_predicates_preserve_cel_matches_and_uncertain_diagnostics() {
    let engine = MdbaseCelEngine::default();
    let values = [
        json!(null),
        json!(false),
        json!(true),
        json!(0),
        json!(2),
        json!(3),
        json!(-3),
        json!(2.5),
        json!(i64::MAX),
        json!(u64::MAX),
        json!("open"),
        json!("closed"),
        json!("OPEN"),
        json!(""),
        json!("é雪"),
        json!("open\0tail"),
        json!([]),
        json!({"status": "open"}),
    ];
    let expressions = [
        "status == 'open'",
        "'open' != status",
        "status < 'open'",
        "note.status >= 'open'",
        "record.status.startsWith('op')",
        "status == true",
        "status > 2",
        "2 <= status",
        "status != 2",
        "file.path.startsWith('é/')",
        "status == 'open' && priority > 2",
        "priority > 2 && status == 'open'",
    ];
    for expression in expressions {
        let program = engine.compile(expression).unwrap();
        for value in &values {
            for priority in [json!(1), json!(3), json!(null), json!("wrong")] {
                let fields = json!({"status": value, "priority": priority});
                let mut bindings = fields
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>();
                bindings.insert("record".into(), fields.clone());
                bindings.insert("note".into(), fields.clone());
                bindings.insert("file".into(), json!({"path": "é/a.md"}));
                let result = engine.evaluate(&program, &bindings);
                let must_retain = result.as_ref().map_or(true, |value| value == &json!(true));
                assert!(
                    !must_retain || selected(expression, &fields, "é/a.md"),
                    "{expression}: {fields}: {result:?}"
                );
            }
        }
        assert!(
            selected(expression, &json!({}), "é/a.md"),
            "missing fields: {expression}"
        );
    }
    assert!(!selected(
        "status == 'open'",
        &json!({"status": "closed"}),
        "a.md"
    ));
    assert!(!selected("priority > 2", &json!({"priority": 1}), "a.md"));
    assert!(selected(
        "status == 'open' && priority > 2",
        &json!({"status": "closed", "priority": "bad"}),
        "a.md"
    ));
}

#[test]
fn sql_literals_are_bound_and_prefixes_are_not_like_patterns() {
    assert!(selected(
        "status == \"x' OR 1=1 --\"",
        &json!({"status": "x' OR 1=1 --"}),
        "a.md"
    ));
    assert!(!selected(
        "status == \"x' OR 1=1 --\"",
        &json!({"status": "other"}),
        "a.md"
    ));
    assert!(selected(
        "file.path.startsWith('é/%_')",
        &json!({}),
        "é/%_name.md"
    ));
    assert!(!selected(
        "file.path.startsWith('é/%_')",
        &json!({}),
        "é/other.md"
    ));
    assert!(selected("file.path.startsWith('')", &json!({}), "雪.md"));
    // JSON/string NUL behavior is intentionally left residual.
    assert!(selected(
        "status == 'open'",
        &json!({"status": "closed\0suffix"}),
        "a.md"
    ));
}

#[test]
fn unsupported_sql_expressions_remain_residual_without_changing_compilation() {
    let engine = MdbaseCelEngine::default();
    for source in [
        "false",
        "status == null",
        "priority > 2.5",
        "priority == 2u",
        "status == 'open' || status == 'done'",
        "!(status == 'open')",
        "status == projection.status",
        "this.status == 'open'",
        "raw.status == 'open'",
        "record.nested.status == 'open'",
        "has(record.status)",
        "['a'].exists(x, x == status)",
        "status == 'open\\u0000tail'",
        "now() == now()",
    ] {
        let program = engine.compile(source).unwrap();
        assert!(program.sql_predicate().is_none(), "{source}");
    }
    let too_many = vec!["status == 'open'"; 17].join(" && ");
    assert!(engine.compile(&too_many).unwrap().sql_predicate().is_none());
    assert!(engine.compile("broken(").is_err());
}

#[test]
fn in_memory_decisions_equal_the_sql_lowering() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let values = [
        json!("open"),
        json!("Open"),
        json!("op"),
        json!(""),
        json!("open\u{0}x"),
        json!("ümlaut"),
        json!("z"),
        json!(1),
        json!(-5),
        json!(9_007_199_254_740_993_i64),
        json!(i64::MAX),
        json!(18_446_744_073_709_551_615_u64),
        json!(1.5),
        json!(2.0),
        json!(true),
        json!(false),
        json!(null),
        json!(["open"]),
        json!({"open": true}),
    ];
    let expressions = [
        "status == 'open'",
        "status != 'open'",
        "status < 'p'",
        "status >= 'op'",
        "status.startsWith('op')",
        "status.startsWith('')",
        "status == 2",
        "status > -6",
        "status <= 9007199254740993",
        "status == true",
        "status != false",
        "file.path.startsWith('notes/')",
        "status == 'open' && file.path == 'notes/a.md'",
        "record.status == 'open'",
        "missing == 'x'",
    ];
    let engine = MdbaseCelEngine::default();
    for expression in expressions {
        let program = engine.compile(expression).unwrap();
        let predicate = program.sql_predicate().unwrap();
        let mut parameters = Vec::new();
        let (columns, valid, matches) =
            predicate.render_columns(&mut parameters, "record.effective_frontmatter_jsonb");
        let columns = columns.join(", ");
        let sql = format!(
            "SELECT COALESCE({valid}, 0), COALESCE({matches}, 0)
             FROM (SELECT path, {columns} FROM (
                 SELECT ?{path} AS path, jsonb(?{json}) AS effective_frontmatter_jsonb) AS record
             ) AS record",
            path = parameters.len() + 1,
            json = parameters.len() + 2,
        );
        for value in &values {
            for path in ["notes/a.md", "other/b.md"] {
                let effective = json!({"status": value});
                let mut bound = parameters.clone();
                bound.push(rusqlite::types::Value::Text(path.to_string()));
                bound.push(rusqlite::types::Value::Text(effective.to_string()));
                let (sql_decided, sql_matched): (i64, i64) = connection
                    .query_row(&sql, rusqlite::params_from_iter(bound), |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .unwrap();
                let expected = (sql_decided != 0).then_some(sql_matched != 0);
                assert_eq!(
                    predicate.decide(path, &effective),
                    expected,
                    "{expression} with {value} at {path}"
                );
            }
        }
    }
}
