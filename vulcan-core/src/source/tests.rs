use super::*;
use std::collections::BTreeSet;

struct Doc {
    id: &'static str,
    path: &'static str,
    tags: &'static [&'static str],
    links: &'static [&'static str],
}

const DOCS: &[Doc] = &[
    Doc {
        id: "a",
        path: "Projects/A.md",
        tags: &["project", "project/active"],
        links: &["b"],
    },
    Doc {
        id: "b",
        path: "Projects/Sub/B.md",
        tags: &["projects"],
        links: &["a", "c"],
    },
    Doc {
        id: "c",
        path: "projects/C.md",
        tags: &["Project"],
        links: &[],
    },
    Doc {
        id: "d",
        path: "Proj%/D.md",
        tags: &["project0", "a_b"],
        links: &["a"],
    },
    Doc {
        id: "e",
        path: "ProjectsX/E.md",
        tags: &["é/ü"],
        links: &["missing"],
    },
    Doc {
        id: "f",
        path: "F.md",
        tags: &[],
        links: &["f"],
    },
    Doc {
        id: "g",
        path: "Projects0/G.md",
        tags: &["project-x"],
        links: &[],
    },
];

/// The meaning of each source, independent of SQL.
fn reference(source: &SourceExpr, doc: &Doc) -> bool {
    match source {
        SourceExpr::Tag(tag) => doc
            .tags
            .iter()
            .any(|candidate| *candidate == tag || candidate.starts_with(&format!("{tag}/"))),
        SourceExpr::Folder(folder) => {
            folder.is_empty()
                || doc
                    .path
                    .as_bytes()
                    .starts_with(format!("{folder}/").as_bytes())
        }
        SourceExpr::Path(path) => doc.path == path,
        SourceExpr::LinksTo(target) => {
            // Only resolved links count.
            doc.links.contains(&target.as_str()) && DOCS.iter().any(|other| other.id == target)
        }
        SourceExpr::LinkedFrom(source) => DOCS
            .iter()
            .any(|other| other.id == source && other.links.contains(&doc.id)),
        SourceExpr::And(children) => children.iter().all(|child| reference(child, doc)),
        SourceExpr::Or(children) => children.iter().any(|child| reference(child, doc)),
        SourceExpr::Not(inner) => !reference(inner, doc),
    }
}

fn connection() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE documents(id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE);
             CREATE TABLE tags(document_id TEXT NOT NULL, tag_text TEXT NOT NULL);
             CREATE TABLE links(source_document_id TEXT NOT NULL, resolved_target_id TEXT);",
        )
        .unwrap();
    for doc in DOCS {
        connection
            .execute("INSERT INTO documents VALUES (?1, ?2)", [doc.id, doc.path])
            .unwrap();
        for tag in doc.tags {
            connection
                .execute("INSERT INTO tags VALUES (?1, ?2)", [doc.id, tag])
                .unwrap();
        }
        for target in doc.links {
            // Unresolved links have a NULL target.
            let resolved = DOCS
                .iter()
                .any(|other| other.id == *target)
                .then_some(*target);
            connection
                .execute(
                    "INSERT INTO links VALUES (?1, ?2)",
                    rusqlite::params![doc.id, resolved],
                )
                .unwrap();
        }
    }
    connection
        .execute("INSERT INTO links VALUES ('c', NULL)", [])
        .unwrap();
    connection
}

fn sql_ids(connection: &rusqlite::Connection, source: &SourceExpr) -> BTreeSet<String> {
    let mut params = Vec::new();
    let columns = SourceColumns {
        id: "documents.id",
        path: "documents.path",
    };
    let clause = source.render_sql(&columns, &mut params);
    let mut statement = connection
        .prepare(&format!(
            "SELECT documents.id FROM documents WHERE {clause}"
        ))
        .unwrap();
    statement
        .query_map(rusqlite::params_from_iter(params), |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn leaves() -> Vec<SourceExpr> {
    let mut leaves = Vec::new();
    for tag in [
        "project",
        "Project",
        "projects",
        "a_b",
        "a%",
        "é",
        "project/active",
        "",
    ] {
        leaves.push(SourceExpr::Tag(tag.to_string()));
    }
    for folder in [
        "Projects",
        "projects",
        "Proj%",
        "Proj_cts",
        "Projects/Sub",
        "",
        "é",
    ] {
        leaves.push(SourceExpr::Folder(folder.to_string()));
    }
    for path in ["F.md", "f.md", "Projects/A.md", "Projects"] {
        leaves.push(SourceExpr::Path(path.to_string()));
    }
    for id in ["a", "c", "f", "missing"] {
        leaves.push(SourceExpr::LinksTo(id.to_string()));
        leaves.push(SourceExpr::LinkedFrom(id.to_string()));
    }
    leaves
}

#[test]
fn sql_selects_exactly_the_reference_documents() {
    let connection = connection();
    let leaves = leaves();
    let mut sources = leaves.clone();
    for (index, left) in leaves.iter().enumerate() {
        let right = &leaves[(index * 7 + 3) % leaves.len()];
        sources.push(SourceExpr::Not(Box::new(left.clone())));
        sources.push(SourceExpr::And(vec![left.clone(), right.clone()]));
        sources.push(SourceExpr::Or(vec![
            left.clone(),
            SourceExpr::Not(Box::new(right.clone())),
        ]));
    }
    sources.push(SourceExpr::And(Vec::new()));
    sources.push(SourceExpr::Or(Vec::new()));
    for source in &sources {
        let expected = DOCS
            .iter()
            .filter(|doc| reference(source, doc))
            .map(|doc| doc.id.to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(sql_ids(&connection, source), expected, "{source:?}");
    }
}

#[test]
fn folders_and_tags_are_byte_exact() {
    let connection = connection();
    let ids = |source: SourceExpr| {
        sql_ids(&connection, &source)
            .into_iter()
            .collect::<Vec<_>>()
    };
    // No case folding, no `LIKE` wildcards, no sibling prefixes.
    assert_eq!(ids(SourceExpr::Folder("Projects".into())), ["a", "b"]);
    assert_eq!(ids(SourceExpr::Folder("Proj%".into())), ["d"]);
    assert!(ids(SourceExpr::Folder("Proj_".into())).is_empty());
    // Nested tags, but not tags sharing a prefix.
    assert_eq!(ids(SourceExpr::Tag("project".into())), ["a"]);
    assert_eq!(ids(SourceExpr::Tag("é".into())), ["e"]);
    // Unresolved links never match, and `NOT` stays exact around them.
    assert_eq!(
        ids(SourceExpr::LinkedFrom("c".into())),
        Vec::<String>::new()
    );
    assert_eq!(
        ids(SourceExpr::Not(Box::new(SourceExpr::LinkedFrom(
            "e".into()
        ))))
        .len(),
        DOCS.len()
    );
}
