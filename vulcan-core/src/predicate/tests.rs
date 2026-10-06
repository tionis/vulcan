use super::*;
use crate::expression::eval::{evaluate, is_truthy, EvalContext};
use crate::expression::parse::Parser;
use crate::properties::NoteRecord;
use serde_json::json;
use std::collections::BTreeMap;

fn note(properties: Value, path: &str, name: &str, ext: &str) -> NoteRecord {
    NoteRecord {
        document_id: "id".to_string(),
        document_path: path.to_string(),
        file_name: name.to_string(),
        file_ext: ext.to_string(),
        file_mtime: 0,
        file_ctime: 0,
        file_size: 0,
        properties,
        tags: Vec::new(),
        links: Vec::new(),
        starred: false,
        inlinks: Vec::new(),
        aliases: Vec::new(),
        frontmatter: json!({}),
        periodic_type: None,
        periodic_date: None,
        list_items: Vec::new(),
        tasks: Vec::new(),
        raw_inline_expressions: Vec::new(),
        inline_expressions: Vec::new(),
    }
}

/// The evaluator's verdict, or `None` when evaluation fails (the frontend
/// then reports a diagnostic; such rows must never be decided).
fn evaluator(source: &str, note: &NoteRecord) -> Option<bool> {
    let expr = Parser::new(source).unwrap().parse().unwrap();
    let formulas = BTreeMap::new();
    evaluate(&expr, &EvalContext::new(note, &formulas))
        .ok()
        .map(|value| is_truthy(&value))
}

fn sql_possible(predicate: &Predicate, note: &NoteRecord) -> bool {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE record(properties TEXT, path TEXT, name TEXT, ext TEXT)")
        .unwrap();
    connection
        .execute(
            "INSERT INTO record VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                note.properties.to_string(),
                note.document_path,
                note.file_name,
                note.file_ext
            ],
        )
        .unwrap();
    let mut params = Vec::new();
    let clause = predicate.render_possible_match(
        Dialect::Dataview,
        &SqlColumns {
            properties: "properties",
            path: "path",
            name: "name",
            ext: "ext",
        },
        &mut params,
    );
    connection
        .query_row(
            &format!("SELECT {clause} FROM record"),
            rusqlite::params_from_iter(params),
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
}

/// Every decided record agrees with the evaluator, and SQL excludes exactly
/// the records the in-memory decider decides as non-matches.
fn check(source: &str, note: &NoteRecord) -> Decision {
    let expr = Parser::new(source).unwrap().parse().unwrap();
    let predicate = Predicate::lower_dataview(&expr);
    let decision = predicate.decide(
        Dialect::Dataview,
        &RecordValues {
            properties: &note.properties,
            path: &note.document_path,
            name: &note.file_name,
            ext: &note.file_ext,
        },
    );
    let truth = evaluator(source, note);
    match decision {
        Decision::Match => assert_eq!(truth, Some(true), "{source} on {}", note.properties),
        Decision::NoMatch => assert_eq!(truth, Some(false), "{source} on {}", note.properties),
        Decision::Undecided => {}
    }
    assert_eq!(
        sql_possible(&predicate, note),
        decision != Decision::NoMatch,
        "{source} on {}",
        note.properties
    );
    decision
}

fn values() -> Vec<Option<Value>> {
    let mut values = vec![None];
    values.extend(
        [
            json!(null),
            json!(true),
            json!(false),
            json!(0),
            json!(1),
            json!(2.5),
            json!(-3),
            json!(9_007_199_254_740_993_u64),
            json!(i64::MAX),
            json!("a"),
            json!("b"),
            json!(""),
            json!("A"),
            json!("é"),
            json!("5"),
            json!("2026-01-01"),
            json!("1 day"),
            json!("[[a]]"),
            json!([]),
            json!(["a"]),
            json!({}),
        ]
        .into_iter()
        .map(Some),
    );
    values
}

fn literals() -> Vec<&'static str> {
    vec![
        "null",
        "true",
        "false",
        "0",
        "1",
        "2.5",
        "-3",
        "9007199254740993",
        "100000000000000000000000",
        "\"a\"",
        "\"b\"",
        "\"\"",
        "\"A\"",
        "\"é\"",
        "\"5\"",
        "\"2026-01-01\"",
        "\"1 day\"",
    ]
}

const OPERATORS: [&str; 6] = ["=", "!=", "<", "<=", ">", ">="];

#[test]
fn dataview_property_atoms_agree_with_the_evaluator_and_sql() {
    let mut decided = 0;
    for value in values() {
        let properties = value.map_or_else(|| json!({}), |value| json!({ "k": value }));
        let note = note(properties, "folder/n.md", "n", "md");
        for literal in literals() {
            for operator in OPERATORS {
                for source in [
                    format!("k {operator} {literal}"),
                    format!("{literal} {operator} k"),
                ] {
                    if check(&source, &note) != Decision::Undecided {
                        decided += 1;
                    }
                }
            }
        }
    }
    // The matrix must exercise decisions, not just undecided atoms.
    assert!(decided > 2_000, "{decided}");
}

#[test]
fn dataview_file_field_atoms_agree_with_the_evaluator_and_sql() {
    for (path, name, ext) in [
        ("folder/n.md", "n", "md"),
        ("Daily/2026-01-01.md", "2026-01-01", "md"),
        ("é/b.canvas", "b", "canvas"),
    ] {
        let note = note(json!({}), path, name, ext);
        for field in ["file.path", "file.name", "file.basename", "file.ext"] {
            for literal in literals() {
                for operator in OPERATORS {
                    check(&format!("{field} {operator} {literal}"), &note);
                }
            }
        }
        assert_eq!(
            check(&format!("file.path = \"{path}\""), &note),
            Decision::Match,
            "{path}"
        );
        assert_eq!(check("file.name = \"other\"", &note), Decision::NoMatch);
    }
}

#[test]
fn unlowered_parts_bound_what_combinations_decide() {
    let note = note(json!({"k": "a", "n": 2}), "folder/n.md", "n", "md");
    for (source, expected) in [
        // Lowered conjunctions and disjunctions.
        ("k = \"a\" && n > 1", Decision::Match),
        ("k = \"b\" && n > 1", Decision::NoMatch),
        ("k = \"b\" || n > 1", Decision::Match),
        ("k = \"b\" || n > 5", Decision::NoMatch),
        // An unlowered conjunct may report a diagnostic, so nothing after it
        // can exclude the record; before it, a non-match short-circuits.
        ("k = \"b\" && length(k) > 0", Decision::NoMatch),
        ("length(k) > 0 && k = \"b\"", Decision::Undecided),
        ("k = \"a\" && length(k) > 0", Decision::Undecided),
        // A disjunction with an unlowered part never excludes.
        ("k = \"b\" || length(k) > 5", Decision::Undecided),
        ("k = \"a\" || length(k) > 5", Decision::Match),
        // Nested groups follow the same rules.
        ("(k = \"b\" || n = 3) && length(k) > 0", Decision::NoMatch),
        ("missing = \"a\" && k = \"b\"", Decision::NoMatch),
        ("missing = \"a\" && k = \"a\"", Decision::Undecided),
    ] {
        assert_eq!(check(source, &note), expected, "{source}");
    }
}

#[test]
fn only_plain_strings_and_finite_numbers_lower() {
    for source in [
        "k = \"2026-01-01\"",
        "k = \"1 day\"",
        "k = date(\"2026-01-01\")",
        "this.k = \"a\"",
        "note.k = \"a\"",
        "file.size = 1",
        "k.length = 1",
        "k = n",
        "!(k = \"a\")",
    ] {
        let expr = Parser::new(source).unwrap().parse().unwrap();
        assert!(!Predicate::lower_dataview(&expr).is_useful(), "{source}");
    }
}

#[test]
fn json_paths_select_exact_keys_with_any_spelling() {
    let note = note(
        json!({"due date": "a", "quo\"te": "b", "back\\slash": "c", "a.b": "d"}),
        "n.md",
        "n",
        "md",
    );
    for source in ["`due date` = \"a\"", "`a.b` = \"d\""] {
        if Parser::new(source).and_then(Parser::parse).is_ok() {
            check(source, &note);
        }
    }
    for (key, literal) in [("quo\"te", "b"), ("back\\slash", "c"), ("a.b", "d")] {
        let predicate = Predicate::Atom(Atom {
            field: Field::Property(key.to_string()),
            comparison: Comparison::Equal,
            literal: Literal::Text(literal.to_string()),
        });
        assert!(sql_possible(&predicate, &note), "{key}");
        let mismatch = Predicate::Atom(Atom {
            field: Field::Property(key.to_string()),
            comparison: Comparison::Equal,
            literal: Literal::Text("other".to_string()),
        });
        assert!(!sql_possible(&mismatch, &note), "{key}");
    }
}
