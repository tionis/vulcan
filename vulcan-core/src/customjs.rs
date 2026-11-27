//! `CustomJS`-compatible vault classes.
//!
//! The Obsidian `CustomJS` plugin loads one JavaScript class per file from
//! configured files and folders and exposes an instance of each as
//! `customJS.<ClassName>`. Vulcan finds the same files here and loads them
//! into `DataviewJS` sessions lazily, the first time a script reads
//! `customJS` or calls `cJS()` (see `CUSTOMJS_PRELUDE` in `dataview_js`).
//! `docs/specs/live-documents.md` (9.42.2) is the contract.

use std::cmp::Ordering;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::CustomJsConfig;
use crate::paths::VaultPaths;
use crate::permissions::PermissionGuard;

/// One class file to load, in load order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomJsSource {
    /// Vault-relative path with `/` separators.
    pub path: String,
    pub source: String,
}

/// A class file that could not be read; it is skipped, as `CustomJS` skips
/// files that fail to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomJsSkippedFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomJsSources {
    pub files: Vec<CustomJsSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CustomJsSkippedFile>,
}

impl CustomJsConfig {
    /// Whether any class files are configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.js_files.iter().any(|file| is_js_path(file.trim())) || !self.js_folder.is_empty()
    }
}

/// The class files `config` selects, in `CustomJS` load order: `js_files`
/// entries, then every vault `.js` file whose path starts with `js_folder`
/// (a plain string prefix, so it is recursive and `scripts` also matches
/// `scripts2/`), all sorted by file name with the directory ignored. A file
/// selected twice loads twice, as in `CustomJS`. Files `guard` cannot read are
/// skipped.
#[must_use]
pub fn load_customjs_sources(
    paths: &VaultPaths,
    config: &CustomJsConfig,
    guard: Option<&dyn PermissionGuard>,
) -> CustomJsSources {
    let mut selected = config
        .js_files
        .iter()
        .map(|file| normalize(file.trim()))
        .filter(|file| is_js_path(file))
        .collect::<Vec<_>>();
    selected.sort();
    if !config.js_folder.is_empty() {
        let prefix = normalize(&config.js_folder);
        selected.extend(
            vault_js_files(paths.vault_root())
                .into_iter()
                .filter(|path| path.starts_with(&prefix)),
        );
    }
    selected.sort_by(|left, right| compare_file_names(basename(left), basename(right)));

    let mut sources = CustomJsSources::default();
    for path in selected {
        if let Some(guard) = guard {
            if let Err(error) = guard.check_read_path(&path) {
                sources.skipped.push(CustomJsSkippedFile {
                    path,
                    reason: error.to_string(),
                });
                continue;
            }
        }
        match std::fs::read_to_string(paths.vault_root().join(&path)) {
            Ok(source) => sources.files.push(CustomJsSource { path, source }),
            Err(error) => sources.skipped.push(CustomJsSkippedFile {
                path,
                reason: error.to_string(),
            }),
        }
    }
    sources
}

// CustomJS compares extensions case-sensitively; `.JS` files do not load.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_js_path(path: &str) -> bool {
    !path.is_empty() && path.ends_with(".js")
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Approximates `String.prototype.localeCompare` for file names, which
/// `CustomJS` sorts with: case-insensitive first, then lowercase before
/// uppercase.
fn compare_file_names(left: &str, right: &str) -> Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| right.cmp(left))
}

/// Every `.js` file in the vault as Obsidian lists vault files: hidden
/// files and folders (`.obsidian`, `.vulcan`, `.git`) are not vault files.
fn vault_js_files(vault_root: &Path) -> Vec<String> {
    let mut builder = ignore::WalkBuilder::new(vault_root);
    builder
        .hidden(true)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(false)
        .require_git(false);
    builder
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(vault_root).ok()?;
            let path = relative.to_str()?.replace('\\', "/");
            is_js_path(&path).then_some(path)
        })
        .collect()
}

/// The DQL function bridge (`[customjs.dql_functions]`): DQL calls such as
/// `fmt(this.price)` run the mapped `CustomJS` method.
///
/// Calls run in their own small JavaScript engine that holds only the
/// `CustomJS` classes: no `dv`, no vault reads, no network, under the vault's
/// memory, stack, and timeout limits. A function therefore sees only its
/// arguments, so a permission-scoped query cannot read past its scope
/// through one. The engine starts on the first call.
// Without the runtime only the configuration is kept, to name the feature
// in the error.
#[cfg_attr(not(feature = "js_runtime"), allow(dead_code))]
pub struct DqlBridge {
    paths: VaultPaths,
    customjs: CustomJsConfig,
    limits: crate::config::JsRuntimeConfig,
    filter: Option<crate::permissions::PermissionFilter>,
    #[cfg(feature = "js_runtime")]
    engine: std::cell::RefCell<Option<Result<bridge::Engine, String>>>,
}

impl DqlBridge {
    /// The bridge for `config`, or `None` when it maps no functions. Class
    /// files outside `filter` are not loaded.
    #[must_use]
    pub fn new(
        paths: &VaultPaths,
        config: &crate::config::VaultConfig,
        filter: Option<&crate::permissions::PermissionFilter>,
    ) -> Option<Self> {
        if config.customjs.dql_functions.is_empty() {
            return None;
        }
        Some(Self {
            paths: paths.clone(),
            customjs: config.customjs.clone(),
            limits: config.js_runtime.clone(),
            filter: filter.cloned(),
            #[cfg(feature = "js_runtime")]
            engine: std::cell::RefCell::new(None),
        })
    }

    #[cfg_attr(not(feature = "js_runtime"), allow(dead_code))]
    fn sources(&self) -> CustomJsSources {
        let mut sources = load_customjs_sources(&self.paths, &self.customjs, None);
        if let Some(filter) = &self.filter {
            let (allowed, denied) = std::mem::take(&mut sources.files)
                .into_iter()
                .partition::<Vec<_>, _>(|file| filter.is_allowed(&file.path));
            sources.files = allowed;
            sources
                .skipped
                .extend(denied.into_iter().map(|file| CustomJsSkippedFile {
                    path: file.path,
                    reason: "not readable under the query's permissions".to_string(),
                }));
        }
        sources
    }
}

impl crate::expression::eval::ExternalFunctions for DqlBridge {
    fn has_function(&self, name: &str) -> bool {
        self.customjs.dql_functions.contains_key(name)
    }

    #[cfg(not(feature = "js_runtime"))]
    fn call_function(
        &self,
        name: &str,
        _args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        Err(format!(
            "DQL function `{name}` is a CustomJS function, which requires the `js_runtime` feature"
        ))
    }

    #[cfg(feature = "js_runtime")]
    fn call_function(
        &self,
        name: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let target = self
            .customjs
            .dql_functions
            .get(name)
            .ok_or_else(|| format!("unknown function `{name}`"))?;
        if !target.contains('.') {
            return Err(format!(
                "DQL function `{name}` must map to `Class.method`, not `{target}`"
            ));
        }
        let mut engine = self.engine.borrow_mut();
        let engine =
            engine.get_or_insert_with(|| bridge::Engine::new(&self.sources(), &self.limits));
        let engine = engine.as_ref().map_err(Clone::clone)?;
        engine
            .call(target, &args)
            .map_err(|error| format!("DQL function `{name}` failed: {error}"))
    }
}

#[cfg(feature = "js_runtime")]
mod bridge {
    use std::time::{Duration, Instant};

    use rquickjs::{CatchResultExt, Context, Runtime};

    use super::CustomJsSources;
    use crate::config::JsRuntimeConfig;

    pub(super) struct Engine {
        runtime: Runtime,
        context: Context,
        timeout: Duration,
    }

    impl Engine {
        pub(super) fn new(
            sources: &CustomJsSources,
            limits: &JsRuntimeConfig,
        ) -> Result<Self, String> {
            let runtime = Runtime::new().map_err(|error| error.to_string())?;
            runtime.set_memory_limit(limits.memory_limit_mb.saturating_mul(1024 * 1024));
            runtime.set_max_stack_size(crate::resource_limits::quickjs_stack_bytes(
                limits.stack_limit_kb.saturating_mul(1024),
            ));
            let context = Context::full(&runtime).map_err(|error| error.to_string())?;
            let sources_json = serde_json::to_string(sources).map_err(|error| error.to_string())?;
            context.with(|ctx| -> Result<(), String> {
                ctx.globals()
                    .set("__vulcan_customjs_sources_json", sources_json)
                    .map_err(|error| error.to_string())?;
                ctx.eval::<(), _>(BRIDGE_PRELUDE)
                    .catch(&ctx)
                    .map_err(|error| error.to_string())?;
                ctx.eval::<(), _>(super::CUSTOMJS_PRELUDE)
                    .catch(&ctx)
                    .map_err(|error| error.to_string())
            })?;
            Ok(Self {
                runtime,
                context,
                timeout: Duration::from_secs(
                    u64::try_from(limits.default_timeout_seconds.max(1)).unwrap_or(u64::MAX),
                ),
            })
        }

        pub(super) fn call(
            &self,
            target: &str,
            args: &[serde_json::Value],
        ) -> Result<serde_json::Value, String> {
            let target = serde_json::to_string(target).map_err(|error| error.to_string())?;
            let args = serde_json::to_string(args).map_err(|error| error.to_string())?;
            let started = Instant::now();
            let timeout = self.timeout;
            self.runtime
                .set_interrupt_handler(Some(Box::new(move || started.elapsed() >= timeout)));
            let result = self.context.with(|ctx| -> Result<String, String> {
                ctx.eval::<String, _>(format!(
                    "JSON.stringify(__vulcanCustomJsCall({target}, {args}) ?? null)"
                ))
                .catch(&ctx)
                .map_err(|error| error.to_string())
            });
            self.runtime.set_interrupt_handler(None);
            serde_json::from_str(&result?).map_err(|error| error.to_string())
        }
    }

    /// What the `CustomJS` prelude needs from its host: the class sources and a
    /// console whose output, like a rendered note's, goes nowhere.
    const BRIDGE_PRELUDE: &str = r"
globalThis.__vulcan_customjs_sources = () => globalThis.__vulcan_customjs_sources_json;
globalThis.console = { log() {}, info() {}, warn() {}, error() {}, debug() {} };
";
}

/// Defines `customJS`, `cJS()`, `forceLoadCustomJS()`, and the bridge's
/// `__vulcanCustomJsCall()` over `__vulcan_customjs_sources()`.
#[cfg_attr(not(feature = "js_runtime"), allow(dead_code))]
pub(crate) const CUSTOMJS_PRELUDE: &str = include_str!("customjs_prelude.js");

#[cfg(test)]
mod tests {
    use super::{compare_file_names, load_customjs_sources};
    use crate::config::CustomJsConfig;
    use crate::VaultPaths;
    use std::cmp::Ordering;

    fn vault(files: &[(&str, &str)]) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().expect("temp dir");
        for (path, source) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("dir");
            std::fs::write(target, source).expect("write");
        }
        let paths = VaultPaths::new(dir.path());
        (dir, paths)
    }

    #[test]
    fn files_load_in_basename_order_from_files_and_folder_prefix() {
        let (_dir, paths) = vault(&[
            ("scripts/b.js", "class B {}"),
            ("scripts/nested/a.js", "class A {}"),
            ("scripts2/c.js", "class C {}"),
            ("other/Z.js", "class Z {}"),
            ("other/skip.md", "not js"),
            (".obsidian/plugins/x.js", "class Hidden {}"),
        ]);
        let config = CustomJsConfig {
            js_files: vec![
                " other/Z.js ".to_string(),
                String::new(),
                "notes.md".to_string(),
            ],
            js_folder: "scripts".to_string(),
            ..CustomJsConfig::default()
        };
        let sources = load_customjs_sources(&paths, &config, None);
        let order = sources
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                "scripts/nested/a.js",
                "scripts/b.js",
                "scripts2/c.js",
                "other/Z.js"
            ]
        );
        assert!(sources.skipped.is_empty());
    }

    #[test]
    fn missing_files_are_skipped_with_a_reason() {
        let (_dir, paths) = vault(&[]);
        let config = CustomJsConfig {
            js_files: vec!["gone.js".to_string()],
            ..CustomJsConfig::default()
        };
        let sources = load_customjs_sources(&paths, &config, None);
        assert!(sources.files.is_empty());
        assert_eq!(sources.skipped[0].path, "gone.js");
    }

    #[test]
    fn unconfigured_vaults_load_nothing() {
        assert!(!CustomJsConfig::default().is_configured());
        let config = CustomJsConfig {
            js_files: vec!["notes.md".to_string()],
            ..CustomJsConfig::default()
        };
        assert!(!config.is_configured());
    }

    #[test]
    fn names_compare_like_locale_compare() {
        assert_eq!(compare_file_names("a.js", "B.js"), Ordering::Less);
        assert_eq!(compare_file_names("b.js", "B.js"), Ordering::Less);
        assert_eq!(compare_file_names("B.js", "a.js"), Ordering::Greater);
    }
}

#[cfg(all(test, feature = "js_runtime"))]
mod bridge_tests {
    use crate::{evaluate_dql, scan_vault, ScanMode, VaultPaths};

    fn vault(config: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".vulcan")).expect(".vulcan");
        std::fs::write(dir.path().join(".vulcan/config.toml"), config).expect("config");
        for (path, source) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("dir");
            std::fs::write(target, source).expect("write");
        }
        let paths = VaultPaths::new(dir.path());
        scan_vault(&paths, ScanMode::Full).expect("scan");
        (dir, paths)
    }

    const CONFIG: &str = concat!(
        "[customjs]\njs_folder = \"scripts\"\n",
        "[customjs.dql_functions]\n",
        "money = \"Format.money\"\n",
        "peek = \"Format.peek\"\n",
        "later = \"Format.later\"\n",
        "missing = \"Nope.run\"\n",
    );

    const FORMAT: &str = r"class Format {
  money(value) { return `${Number(value).toFixed(2)} EUR`; }
  peek() { return typeof dv; }
  async later() { return 1; }
}";

    #[test]
    fn dql_calls_mapped_customjs_methods() {
        let (_dir, paths) = vault(
            CONFIG,
            &[
                ("scripts/Format.js", FORMAT),
                ("Shop.md", "---\nprice: 12.5\n---\n"),
            ],
        );
        let result = evaluate_dql(&paths, "TABLE money(price) AS cost WHERE price", None)
            .expect("query should evaluate");
        assert_eq!(result.rows[0]["cost"], serde_json::json!("12.50 EUR"));
    }

    #[test]
    fn bridge_functions_see_only_their_arguments() {
        let (_dir, paths) = vault(
            CONFIG,
            &[
                ("scripts/Format.js", FORMAT),
                ("Shop.md", "---\nprice: 12.5\n---\n"),
            ],
        );
        let result = evaluate_dql(&paths, "TABLE peek() AS seen WHERE price", None)
            .expect("query should evaluate");
        assert_eq!(result.rows[0]["seen"], serde_json::json!("undefined"));
    }

    #[test]
    fn async_methods_and_missing_classes_are_errors() {
        let (_dir, paths) = vault(
            CONFIG,
            &[
                ("scripts/Format.js", FORMAT),
                (
                    "Shop.md",
                    "---\nprice: 12.5\n---\nAsync: `= later()` Missing: `= missing()`\n",
                ),
            ],
        );
        let note_index = crate::properties::load_note_index(&paths).expect("index");
        let note = note_index
            .values()
            .find(|note| note.document_path == "Shop.md")
            .expect("note");
        let config = crate::load_vault_config(&paths).config;
        let bridge = super::DqlBridge::new(&paths, &config, None).expect("bridge");
        let results = crate::evaluate_note_inline_expressions_with_functions(
            note,
            &note_index,
            Some(&bridge),
        );
        let errors = results
            .iter()
            .map(|result| result.error.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        assert!(errors[0].contains("not a Promise"), "{errors:?}");
        assert!(errors[1].contains("`Nope` is not loaded"), "{errors:?}");
    }

    #[test]
    fn rendered_dataviewjs_reaches_classes_through_customjs_and_cjs() {
        let (_dir, paths) = vault(
            "[dataview]\nenable_inline_dataview_js = true\n[customjs]\njs_files = [\"scripts/Format.js\"]\n",
            &[("scripts/Format.js", FORMAT)],
        );
        let source = concat!(
            "Inline: `$= customJS.Format.money(2)`\n\n",
            "```dataviewjs\nconst { Format } = await cJS();\ndv.paragraph(Format.money(4));\n```\n",
        );
        let markdown = crate::render_evaluated_markdown(
            &paths,
            source,
            &crate::EvaluatedMarkdownOptions {
                source_path: Some("Shop.md"),
                dataview_js: crate::DataviewJsRenderPolicy::On,
                ..crate::EvaluatedMarkdownOptions::default()
            },
        )
        .markdown;
        assert!(markdown.contains("Inline: 2.00 EUR"), "{markdown}");
        assert!(markdown.contains("4.00 EUR"), "{markdown}");
    }

    #[test]
    fn unmapped_vaults_have_no_bridge() {
        let (_dir, paths) = vault("", &[]);
        let config = crate::load_vault_config(&paths).config;
        assert!(super::DqlBridge::new(&paths, &config, None).is_none());
    }
}
