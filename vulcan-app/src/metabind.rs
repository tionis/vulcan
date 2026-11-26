//! Meta Bind fields (`vulcan metabind`): list a note's fields with their
//! current values, and plan the frontmatter writes of `set` (one bound
//! value, validated by the inputs bound to it) and `apply` (the write-backs
//! of `VIEW[...][type:target]` fields). Rendering never writes; see
//! `vulcan_core::metabind` and `docs/specs/live-documents.md` (9.42.3).

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::Value;
use vulcan_core::metabind::{
    fields_in_source, inputs_bound_to, resolve_target_note, view_write_backs,
    write_target_in_source, BindTarget, DataviewJsScripts, MetaBindEnv, MetaBindField,
    MetaBindStorage,
};
use vulcan_core::{NoteRecord, PermissionGuard, ProfilePermissionGuard, VaultConfig, VaultPaths};

use crate::AppError;

mod press;
pub use press::{
    press_metabind_button, MetaBindActionReport, MetaBindPressOptions, MetaBindPressReport,
    MetaBindWriter,
};
pub use vulcan_core::metabind::buttons::ButtonSelector;

/// A note's fields.
#[derive(Debug, Clone, Serialize)]
pub struct MetaBindReport {
    pub path: String,
    pub enabled: bool,
    pub fields: Vec<MetaBindField>,
}

/// One frontmatter value a plan changes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetaBindChange {
    /// The note the value lives in.
    pub path: String,
    pub target: String,
    pub before: Option<Value>,
    pub after: Value,
}

/// The notes a plan rewrites: `(path, before, after)` sources, and the
/// value changes they carry.
#[derive(Debug, Clone, Default)]
pub struct MetaBindWritePlan {
    pub writes: Vec<MetaBindFileWrite>,
    pub changes: Vec<MetaBindChange>,
    /// Write-backs `apply` leaves alone, with the reason.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MetaBindFileWrite {
    pub path: String,
    pub before: String,
    pub after: String,
    /// A note the write creates (`before` is empty).
    pub created: bool,
}

/// The render environment `vulcan metabind` uses: the device clock, the
/// vault's files, and JS views under the vault's `DataviewJS` policy
/// (full evaluation for a trusted vault, the strict sandbox otherwise;
/// never writing).
fn env<'a>(paths: &'a VaultPaths, scripts: &'a DataviewJsScripts<'a>) -> MetaBindEnv<'a> {
    MetaBindEnv {
        paths: Some(paths),
        scripts: Some(scripts),
        ..MetaBindEnv::with_clock()
    }
}

fn scripts<'a>(
    paths: &'a VaultPaths,
    path: &'a str,
    permission_profile: Option<&str>,
) -> DataviewJsScripts<'a> {
    let policy = vulcan_core::vault_dataview_js_policy(paths, None);
    DataviewJsScripts::new(
        paths,
        Some(path),
        vulcan_core::render_policy_options(policy, permission_profile, false),
    )
}

struct Scope {
    note_index: Option<HashMap<String, NoteRecord>>,
    guard: Option<ProfilePermissionGuard>,
}

impl Scope {
    fn load(
        paths: &VaultPaths,
        path: &str,
        permission_profile: Option<&str>,
    ) -> Result<Self, AppError> {
        if permission_profile.is_none() {
            return Ok(Self {
                note_index: vulcan_core::properties::load_note_index(paths).ok(),
                guard: None,
            });
        }
        let selection = vulcan_core::resolve_permission_profile(paths, permission_profile)
            .map_err(AppError::operation)?;
        let guard = ProfilePermissionGuard::new(paths, selection);
        guard.check_read_path(path).map_err(AppError::operation)?;
        Ok(Self {
            note_index: vulcan_core::properties::load_note_index_with_guard(paths, &guard).ok(),
            guard: Some(guard),
        })
    }
}

/// The fields of `source`, the note at `path`, with values read under
/// `permission_profile`.
pub fn list_metabind_fields(
    paths: &VaultPaths,
    config: &VaultConfig,
    path: &str,
    source: &str,
    permission_profile: Option<&str>,
) -> Result<MetaBindReport, AppError> {
    let scope = Scope::load(paths, path, permission_profile)?;
    Ok(MetaBindReport {
        path: path.to_string(),
        enabled: config.metabind.enabled,
        fields: fields_in_source(
            config,
            scope.note_index.as_ref(),
            Some(path),
            source,
            &env(paths, &scripts(paths, path, permission_profile)),
        ),
    })
}

/// Plans writing `value` (text, as an input would take it) to `target` in
/// the note at `path`. The note must hold an input bound to `target`, and
/// every input bound to it must accept the value.
pub fn plan_metabind_set(
    paths: &VaultPaths,
    config: &VaultConfig,
    path: &str,
    source: &str,
    target: &str,
    value: &str,
    permission_profile: Option<&str>,
) -> Result<MetaBindWritePlan, AppError> {
    let scope = Scope::load(paths, path, permission_profile)?;
    let target = BindTarget::parse(target).map_err(AppError::operation)?;
    let fields = fields_in_source(
        config,
        scope.note_index.as_ref(),
        Some(path),
        source,
        &env(paths, &scripts(paths, path, permission_profile)),
    );
    let inputs = inputs_bound_to(&fields, &target);
    let Some(first) = inputs.first() else {
        return Err(AppError::operation(format!(
            "no input in {path} is bound to `{target}`; `vulcan note set-property` sets other properties"
        )));
    };
    let parsed = first.parse_value(value).map_err(AppError::operation)?;
    for input in &inputs[1..] {
        input.parse_value(value).map_err(|error| {
            AppError::operation(format!(
                "another `{}` input refuses it: {error}",
                input.input_type
            ))
        })?;
    }
    let mut plan = MetaBindWritePlan::default();
    let mut sources = BTreeMap::new();
    write_value(
        paths,
        &scope,
        path,
        source,
        &target,
        parsed,
        &mut sources,
        &mut plan,
    )?;
    Ok(finish(plan, sources))
}

/// Plans writing the results of the note's views that name a frontmatter
/// write-back target (`VIEW[...][math:target]`), in document order.
pub fn plan_metabind_apply(
    paths: &VaultPaths,
    config: &VaultConfig,
    path: &str,
    source: &str,
    permission_profile: Option<&str>,
) -> Result<MetaBindWritePlan, AppError> {
    let scope = Scope::load(paths, path, permission_profile)?;
    let fields = fields_in_source(
        config,
        scope.note_index.as_ref(),
        Some(path),
        source,
        &env(paths, &scripts(paths, path, permission_profile)),
    );
    let mut plan = MetaBindWritePlan::default();
    let mut sources = BTreeMap::new();
    for write_back in view_write_backs(&fields) {
        if let Err(error) = write_value(
            paths,
            &scope,
            path,
            source,
            &write_back.target,
            write_back.value,
            &mut sources,
            &mut plan,
        ) {
            plan.skipped.push(format!(
                "line {}: `{}`: {error}",
                write_back.line_number, write_back.target
            ));
        }
    }
    Ok(finish(plan, sources))
}

#[allow(clippy::too_many_arguments)]
fn write_value(
    paths: &VaultPaths,
    scope: &Scope,
    path: &str,
    source: &str,
    target: &BindTarget,
    value: Value,
    sources: &mut BTreeMap<String, (String, String)>,
    plan: &mut MetaBindWritePlan,
) -> Result<(), AppError> {
    if target.storage != MetaBindStorage::Frontmatter {
        return Err(AppError::operation(
            "memory values are never stored, in Meta Bind or in Vulcan",
        ));
    }
    let file = match &target.file {
        None => path.to_string(),
        Some(file) => {
            let index = scope.note_index.as_ref().ok_or_else(|| {
                AppError::operation("the vault is not indexed; run `vulcan scan`")
            })?;
            resolve_target_note(index, path, file)
                .map(|note| note.document_path.clone())
                .ok_or_else(|| AppError::operation(format!("no note `{file}`")))?
        }
    };
    if let Some(guard) = &scope.guard {
        guard.check_write_path(&file).map_err(AppError::operation)?;
    }
    if !sources.contains_key(&file) {
        let original = if file == path {
            source.to_string()
        } else {
            std::fs::read_to_string(paths.vault_root().join(&file)).map_err(AppError::operation)?
        };
        sources.insert(file.clone(), (original.clone(), original));
    }
    let entry = sources.get_mut(&file).expect("source loaded");
    let local = BindTarget {
        file: None,
        ..target.clone()
    };
    let parsed = vulcan_core::parse_document(&entry.1, &VaultConfig::default());
    let before = vulcan_core::metabind::read_path(
        &vulcan_core::metabind::frontmatter_json(&parsed),
        &local.path,
    )
    .cloned();
    if let Some(updated) =
        write_target_in_source(&entry.1, &file, &local, &value).map_err(AppError::operation)?
    {
        entry.1 = updated;
        plan.changes.push(MetaBindChange {
            path: file,
            target: target.to_string(),
            before,
            after: value,
        });
    }
    Ok(())
}

fn finish(
    mut plan: MetaBindWritePlan,
    sources: BTreeMap<String, (String, String)>,
) -> MetaBindWritePlan {
    plan.writes = sources
        .into_iter()
        .filter(|(_, (before, after))| before != after)
        .map(|(path, (before, after))| MetaBindFileWrite {
            path,
            before,
            after,
            created: false,
        })
        .collect();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(files: &[(&str, &str)]) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".vulcan")).expect(".vulcan");
        for (path, source) in files {
            std::fs::write(dir.path().join(path), source).expect("write");
        }
        let paths = VaultPaths::new(dir.path());
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).expect("scan");
        (dir, paths)
    }

    #[test]
    fn set_validates_against_bound_inputs_and_writes_other_notes() {
        let note = concat!(
            "---\nlevel: 2\n---\n",
            "`INPUT[slider(minValue(1), maxValue(5)):level]` ",
            "`INPUT[inlineSelect(option(red), option(blue)):Stats#color]`\n",
        );
        let (_dir, paths) = vault(&[("Hero.md", note), ("Stats.md", "---\ncolor: red\n---\n")]);
        let config = VaultConfig::default();
        let plan =
            plan_metabind_set(&paths, &config, "Hero.md", note, "level", "4", None).expect("plan");
        assert_eq!(plan.writes.len(), 1);
        assert!(plan.writes[0].after.contains("level: 4"));
        assert_eq!(plan.changes[0].before, Some(Value::from(2)));

        let error = plan_metabind_set(&paths, &config, "Hero.md", note, "level", "9", None)
            .expect_err("out of range");
        assert!(error.to_string().contains("between 1 and 5"), "{error}");
        assert!(
            plan_metabind_set(&paths, &config, "Hero.md", note, "title", "x", None).is_err(),
            "unbound targets are refused"
        );

        let plan = plan_metabind_set(
            &paths,
            &config,
            "Hero.md",
            note,
            "Stats#color",
            "blue",
            None,
        )
        .expect("plan");
        assert_eq!(plan.writes[0].path, "Stats.md");
        assert!(plan.writes[0].after.contains("color: blue"));
    }

    #[test]
    fn option_queries_offer_matching_notes_and_images() {
        let note = concat!(
            "`INPUT[suggester(optionQuery(#npc)):who]` ",
            "`INPUT[suggester(optionQuery(\"People\"), useLinks(partial)):folk]` ",
            "`INPUT[suggester(optionQuery(#npc and -\"People\"), useLinks(false)):other]`\n",
            "```meta-bind\nINPUT[imageSuggester(optionQuery(\"art\")):portrait]\n```\n",
        );
        let (dir, paths) = vault(&[
            ("Hub.md", note),
            ("Bob.md", "#npc\n"),
            ("Ann.md", "#npc/villain\n"),
            ("Cat.md", "no tags\n"),
        ]);
        std::fs::create_dir_all(dir.path().join("People")).expect("dir");
        std::fs::write(dir.path().join("People/Dee.md"), "#npc\n").expect("write");
        std::fs::create_dir_all(dir.path().join("art")).expect("dir");
        std::fs::write(dir.path().join("art/face.png"), b"png").expect("write");
        std::fs::write(dir.path().join("art/notes.txt"), b"txt").expect("write");
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).expect("scan");
        let config = VaultConfig::default();
        let report = list_metabind_fields(&paths, &config, "Hub.md", note, None).expect("list");
        let options = |index: usize| match &report.fields[index].outcome {
            vulcan_core::metabind::MetaBindOutcome::Input { settings, .. } => settings
                .options
                .iter()
                .map(|option| option.value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>(),
            other => panic!("not an input: {other:?}"),
        };
        assert_eq!(
            options(0),
            ["[[Ann.md|Ann]]", "[[Bob.md|Bob]]", "[[People/Dee.md|Dee]]"]
        );
        assert_eq!(options(1), ["[[Dee]]"]);
        assert_eq!(options(2), ["Ann", "Bob"]);
        assert_eq!(options(3), ["art/face.png"]);

        let plan = plan_metabind_set(&paths, &config, "Hub.md", note, "other", "Bob", None)
            .expect("an offered value");
        assert!(plan.writes[0].after.contains("other: Bob"));
        assert!(
            plan_metabind_set(&paths, &config, "Hub.md", note, "other", "Cat", None).is_err(),
            "values outside the query are refused"
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn apply_writes_js_view_results_when_js_is_enabled() {
        let note = concat!(
            "---\nprice: 4\n---\n",
            "```meta-bind-js-view\n{price} as p\nsave to {doubled}\n---\nreturn context.bound.p * 2;\n```\n",
        );
        let (_dir, paths) = vault(&[("Shop.md", note)]);
        let mut config = VaultConfig::default();
        let plan = plan_metabind_apply(&paths, &config, "Shop.md", note, None).expect("plan");
        assert!(plan.writes.is_empty(), "JS views need metabind.enable_js");
        config.metabind.enable_js = true;
        let plan = plan_metabind_apply(&paths, &config, "Shop.md", note, None).expect("plan");
        assert!(
            plan.writes[0].after.contains("doubled: 8"),
            "{:?}",
            plan.writes
        );
    }

    #[test]
    fn apply_writes_view_results_in_document_order() {
        let note = concat!(
            "---\nprice: 4\ncount: 3\n---\n",
            "`VIEW[{price} {count}][text:label]` `VIEW[{label}!][text:shout]` ",
            "`VIEW[{price}][text:memory^tmp]`\n",
        );
        let (_dir, paths) = vault(&[("Shop.md", note)]);
        let plan = plan_metabind_apply(&paths, &VaultConfig::default(), "Shop.md", note, None)
            .expect("plan");
        let after = &plan.writes[0].after;
        assert!(after.contains("label: 4 3"), "{after}");
        assert!(after.contains("shout: 4 3!"), "{after}");
        assert_eq!(plan.changes.len(), 2, "memory targets are not written");
    }
}
