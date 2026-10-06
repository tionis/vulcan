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

fn universe(count: usize) -> HashMap<String, NoteRecord> {
    (0..count)
        .map(|index| {
            let note = note(&format!("n{index}.md"));
            (note.file_name.clone(), note)
        })
        .collect()
}

#[test]
fn hydrates_dereferenced_notes_once_and_borrows_hydrated_ones() {
    let notes = universe(4);
    let hydrated = HashSet::from(["n0.md".to_string()]);
    let calls = RefCell::new(Vec::new());
    let lookup = LazyNoteLookup::new(
        &notes,
        &hydrated,
        Box::new(|batch| {
            calls.borrow_mut().push(
                batch
                    .iter()
                    .map(|note| note.document_path.clone())
                    .collect::<Vec<_>>(),
            );
            Ok(batch
                .into_iter()
                .map(|mut note| {
                    note.tags.push("#hydrated".to_string());
                    note
                })
                .collect())
        }),
    );
    // Already hydrated notes are returned as they are.
    assert!(lookup.hydrated(&notes["n0"]).tags.is_empty());
    assert!(calls.borrow().is_empty());
    // A dereference hydrates just that note, once.
    assert_eq!(lookup.hydrated(&notes["n1"]).tags, ["#hydrated"]);
    assert_eq!(lookup.hydrated(&notes["n1"]).tags, ["#hydrated"]);
    assert_eq!(*calls.borrow(), [vec!["n1.md".to_string()]]);
    // Notes outside the universe are returned unchanged.
    let outside = note("other.md");
    assert!(lookup.hydrated(&outside).tags.is_empty());
    assert!(lookup.take_error().is_none());
}

#[test]
fn many_dereferences_hydrate_the_rest_in_one_batch() {
    let notes = universe(LAZY_HYDRATION_LIMIT + 10);
    let hydrated = HashSet::new();
    let calls = RefCell::new(Vec::new());
    let lookup = LazyNoteLookup::new(
        &notes,
        &hydrated,
        Box::new(|batch| {
            calls.borrow_mut().push(batch.len());
            Ok(batch)
        }),
    );
    let mut keys = notes.keys().collect::<Vec<_>>();
    keys.sort();
    for key in &keys {
        let _ = lookup.hydrated(&notes[*key]);
    }
    let calls = calls.borrow();
    // One call per note up to the limit, then every remaining note at once.
    assert_eq!(calls.len(), LAZY_HYDRATION_LIMIT + 1);
    assert!(calls[..LAZY_HYDRATION_LIMIT].iter().all(|size| *size == 1));
    assert_eq!(
        calls[LAZY_HYDRATION_LIMIT],
        keys.len() - LAZY_HYDRATION_LIMIT
    );
}

#[test]
fn hydration_failures_are_reported_not_hidden() {
    let notes = universe(2);
    let hydrated = HashSet::new();
    let lookup = LazyNoteLookup::new(
        &notes,
        &hydrated,
        Box::new(|_| Err(PropertyError::CacheMissing)),
    );
    let _ = lookup.hydrated(&notes["n0"]);
    assert!(matches!(
        lookup.take_error(),
        Some(PropertyError::CacheMissing)
    ));
    assert!(lookup.take_error().is_none());
}
