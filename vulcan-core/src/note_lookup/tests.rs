use super::*;
use serde_json::json;
use std::cell::RefCell;

fn note(path: &str) -> NoteRecord {
    let name = path.trim_end_matches(".md").to_string();
    NoteRecord {
        document_id: format!("id-{name}"),
        document_path: path.to_string(),
        file_name: name,
        file_ext: "md".to_string(),
        file_mtime: 0,
        file_ctime: 0,
        file_size: 0,
        properties: json!({}),
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

fn identities(count: usize) -> Vec<IndexedIdentity> {
    (0..count)
        .map(|index| IndexedIdentity {
            path: format!("n{index}.md"),
            key: format!("n{index}"),
            file_name: format!("n{index}"),
            aliases: vec![format!("alias{index}")],
            row_version: 0,
            document_id: String::new(),
        })
        .collect()
}

struct Calls {
    stored: RefCell<Vec<usize>>,
    hydrated: RefCell<Vec<usize>>,
}

fn lookup(count: usize, calls: &Calls) -> IndexedNoteLookup<'_> {
    IndexedNoteLookup::new(
        identities(count),
        Box::new(move |paths: Option<&[&str]>| {
            let paths = paths.map_or_else(
                || {
                    (0..count)
                        .map(|index| format!("n{index}.md"))
                        .collect::<Vec<_>>()
                },
                |paths| paths.iter().map(ToString::to_string).collect(),
            );
            calls.stored.borrow_mut().push(paths.len());
            Ok(paths.iter().map(|path| Arc::new(note(path))).collect())
        }),
        Box::new(move |notes| {
            calls.hydrated.borrow_mut().push(notes.len());
            Ok(notes
                .into_iter()
                .map(|mut note| {
                    Arc::make_mut(&mut note).tags.push("#hydrated".to_string());
                    note
                })
                .collect())
        }),
    )
}

fn calls() -> Calls {
    Calls {
        stored: RefCell::new(Vec::new()),
        hydrated: RefCell::new(Vec::new()),
    }
}

#[test]
fn resolves_links_from_identities_without_loading_other_notes() {
    let calls = calls();
    let lookup = lookup(4, &calls);
    // Resolution reads identity facts; only the resolved note loads.
    let resolved = lookup.resolve("n0.md", "alias2").unwrap();
    assert_eq!(resolved.document_path, "n2.md");
    assert_eq!(resolved.aliases, ["alias2"]);
    assert_eq!(*calls.stored.borrow(), [1]);
    assert!(calls.hydrated.borrow().is_empty());
    assert_eq!(lookup.paths().count(), 4);
    // A file-object dereference hydrates just that note, once.
    assert_eq!(lookup.hydrated(resolved).tags, ["#hydrated"]);
    assert_eq!(lookup.hydrated(resolved).tags, ["#hydrated"]);
    assert_eq!(*calls.hydrated.borrow(), [1]);
    // Notes outside the universe are returned unchanged.
    let outside = note("other.md");
    assert!(lookup.hydrated(&outside).tags.is_empty());
    assert!(lookup.take_error().is_none());
}

#[test]
fn prefetches_load_in_batches_and_many_misses_load_the_rest() {
    let calls = calls();
    let lookup = lookup(LAZY_LOAD_LIMIT + 10, &calls);
    lookup.prefetch_hydrated(["n0.md", "n1.md", "n2.md"]);
    assert_eq!(*calls.stored.borrow(), [3]);
    assert_eq!(*calls.hydrated.borrow(), [3]);
    assert!(lookup.is_hydrated("n1.md"));
    for index in 3..(LAZY_LOAD_LIMIT + 10) {
        let _ = lookup.note_at(&format!("n{index}.md"));
    }
    let stored = calls.stored.borrow();
    // One batch, then one load per miss up to the limit, then the rest.
    assert_eq!(stored.len(), 1 + LAZY_LOAD_LIMIT + 1);
    assert_eq!(stored[1..=LAZY_LOAD_LIMIT], vec![1; LAZY_LOAD_LIMIT][..]);
    assert_eq!(*stored.last().unwrap(), 10 - 3);
}

#[test]
fn loading_most_of_the_universe_scans_it() {
    let calls = calls();
    let lookup = lookup(10, &calls);
    lookup.prefetch_stored(
        (0..8)
            .map(|index| format!("n{index}.md"))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str),
    );
    // Eight of ten notes load through one scan of every readable note.
    assert_eq!(*calls.stored.borrow(), [10]);
    assert!(lookup.note_at("n9.md").is_some());
    assert_eq!(*calls.stored.borrow(), [10]);
}

#[test]
fn load_failures_are_reported_not_hidden() {
    let lookup = IndexedNoteLookup::new(
        identities(2),
        Box::new(|_| Err(PropertyError::CacheMissing)),
        Box::new(Ok),
    );
    assert!(lookup.note_at("n0.md").is_none());
    assert!(matches!(
        lookup.take_error(),
        Some(PropertyError::CacheMissing)
    ));
    assert!(lookup.take_error().is_none());
}
