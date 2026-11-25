//! `CustomJS` workflows: list the vault's classes and run `invoke()` scripts
//! (`vulcan plugin customjs`). Classes load as in `DataviewJS` sessions; see
//! `vulcan_core::customjs` and `docs/specs/live-documents.md` (9.42.2).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vulcan_core::config::JsRuntimeSandbox;
use vulcan_core::customjs::{load_customjs_sources, CustomJsSkippedFile};
use vulcan_core::{
    load_vault_config, DataviewJsEvalOptions, DataviewJsResult, DataviewJsSession, VaultPaths,
    Verbosity,
};

use crate::mdbase::mdbase_js_mutation_committer;
use crate::plugins::require_trusted_plugin_execution;
use crate::tools::build_custom_tool_js_registry;
use crate::AppError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomJsClass {
    pub name: String,
    /// The class defines `invoke()`.
    pub invocable: bool,
    /// Listed in `customjs.invocable_scripts`.
    pub registered: bool,
    /// Listed in `customjs.startup_scripts`.
    pub startup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomJsReport {
    /// Class files in load order.
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CustomJsSkippedFile>,
    pub classes: Vec<CustomJsClass>,
    /// Files that failed to evaluate, as `CustomJS` reports them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dql_functions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomJsInvocation {
    pub class: String,
    #[serde(flatten)]
    pub result: DataviewJsResult,
}

#[derive(Debug, Deserialize)]
struct LoadedClasses {
    classes: Vec<LoadedClass>,
    errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LoadedClass {
    name: String,
    invocable: bool,
}

/// Load the classes in the strict sandbox, read-only, and report them.
/// Loading runs the classes' constructors, which can only compute there, so
/// listing needs no trust.
pub fn build_customjs_report(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
) -> Result<CustomJsReport, AppError> {
    let config = load_vault_config(paths).config.customjs;
    let sources = load_customjs_sources(paths, &config, None);
    let files = sources
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let loaded = if config.is_configured() {
        let session = DataviewJsSession::new(
            paths,
            None,
            DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Strict),
                read_only: true,
                permission_profile: permission_profile.map(ToOwned::to_owned),
                ..DataviewJsEvalOptions::default()
            },
        )
        .map_err(AppError::operation)?;
        let result = session
            .evaluate(LIST_CLASSES)
            .map_err(AppError::operation)?;
        serde_json::from_value::<LoadedClasses>(result.value.unwrap_or_default())
            .map_err(AppError::operation)?
    } else {
        LoadedClasses {
            classes: Vec::new(),
            errors: Vec::new(),
        }
    };
    let classes = loaded
        .classes
        .into_iter()
        .map(|class| CustomJsClass {
            registered: config.invocable_scripts.contains(&class.name),
            startup: config.startup_scripts.contains(&class.name),
            name: class.name,
            invocable: class.invocable,
        })
        .collect();
    Ok(CustomJsReport {
        files,
        skipped: sources.skipped,
        classes,
        errors: loaded.errors,
        dql_functions: config.dql_functions,
    })
}

/// Run `customJS.<class>.invoke()` at the vault's sandbox tier, as a plugin
/// runs; it may write. `dry_run` refuses every write instead.
pub fn invoke_customjs(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    class: &str,
    dry_run: bool,
) -> Result<CustomJsInvocation, AppError> {
    require_trusted_plugin_execution(paths, Some(&format!("CustomJS `{class}`")))?;
    let session = invocation_session(paths, permission_profile, dry_run)?;
    invoke_in(&session, class)
}

/// Run every `customjs.startup_scripts` class's `invoke()`, in order, in one
/// session, stopping at the first failure as an error.
pub fn run_customjs_startup(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    dry_run: bool,
) -> Result<Vec<CustomJsInvocation>, AppError> {
    let config = load_vault_config(paths).config.customjs;
    if config.startup_scripts.is_empty() {
        return Ok(Vec::new());
    }
    require_trusted_plugin_execution(paths, Some("CustomJS startup scripts"))?;
    let session = invocation_session(paths, permission_profile, dry_run)?;
    config
        .startup_scripts
        .iter()
        .map(|class| invoke_in(&session, class))
        .collect()
}

pub(crate) fn invocation_session(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    dry_run: bool,
) -> Result<DataviewJsSession, AppError> {
    DataviewJsSession::new(
        paths,
        None,
        DataviewJsEvalOptions {
            permission_profile: permission_profile.map(ToOwned::to_owned),
            read_only: dry_run,
            mutation_committer: Some(mdbase_js_mutation_committer(
                paths,
                permission_profile,
                Verbosity::Quiet,
            )),
            tool_registry: Some(build_custom_tool_js_registry(
                paths,
                permission_profile,
                "customjs.invoke",
            )),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(AppError::operation)
}

fn invoke_in(session: &DataviewJsSession, class: &str) -> Result<CustomJsInvocation, AppError> {
    let name = serde_json::to_string(class).map_err(AppError::operation)?;
    // `invoke()` is usually async: start it, let the session drain its
    // jobs, then rethrow a rejection.
    let mut result = session
        .evaluate(&format!(
            r#"(() => {{
  const target = customJS[{name}];
  if (target === undefined) {{
    throw new Error(`Script '${{{name}}}' is not defined`);
  }}
  if (typeof target.invoke !== "function") {{
    throw new Error(`Script '${{{name}}}' is not invocable`);
  }}
  globalThis.__vulcanInvocation = {{}};
  Promise.resolve(target.invoke()).then(
    (value) => {{ globalThis.__vulcanInvocation.value = value; }},
    (error) => {{ globalThis.__vulcanInvocation.error = error; }},
  );
}})()"#
        ))
        .map_err(AppError::operation)?;
    let settled = session
        .evaluate(
            "(() => { const { error, value } = globalThis.__vulcanInvocation; \
             if (error !== undefined) { throw error; } return value; })()",
        )
        .map_err(|error| AppError::operation(format!("Script '{class}' failed: {error}")))?;
    result.outputs.extend(settled.outputs);
    result.value = settled.value;
    Ok(CustomJsInvocation {
        class: class.to_string(),
        result,
    })
}

/// Load `customJS` with console errors captured, and describe its classes.
const LIST_CLASSES: &str = r#"(() => {
  const errors = [];
  const original = console.error;
  console.error = (...args) => errors.push(args.map(String).join(" "));
  let loaded;
  try {
    loaded = customJS;
  } finally {
    console.error = original;
  }
  const classes = Object.keys(loaded)
    .filter((key) => key !== "state" && !/^create.+Instance$/.test(key))
    .sort()
    .map((name) => ({
      name,
      invocable: typeof loaded[name]?.invoke === "function",
    }));
  return { classes, errors };
})()"#;

#[cfg(all(test, feature = "js_runtime"))]
mod tests {
    use super::{build_customjs_report, invoke_customjs, run_customjs_startup};
    use crate::trust::IsolatedTrust;
    use std::fs;
    use vulcan_core::{initialize_vulcan_dir, VaultPaths};

    fn vault(config: &str, files: &[(&str, &str)]) -> (tempfile::TempDir, VaultPaths) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(paths.vault_root().join(".vulcan/config.toml"), config)
            .expect("config should write");
        for (path, source) in files {
            let target = paths.vault_root().join(path);
            fs::create_dir_all(target.parent().expect("parent")).expect("dir");
            fs::write(target, source).expect("file should write");
        }
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).expect("scan");
        (dir, paths)
    }

    #[test]
    fn reports_classes_load_errors_and_registration() {
        let (_dir, paths) = vault(
            "[customjs]\njs_folder = \"scripts\"\ninvocable_scripts = [\"Hello\"]\n",
            &[
                (
                    "scripts/Hello.js",
                    "class Hello { greet(name) { return `hi ${name}`; } async invoke() {} }",
                ),
                (
                    "scripts/Util.js",
                    "class Util { twice(x) { return x * 2; } }",
                ),
                ("scripts/Broken.js", "this is not a class"),
            ],
        );
        let report = build_customjs_report(&paths, None).expect("report");
        assert_eq!(report.files.len(), 3);
        let names = report
            .classes
            .iter()
            .map(|class| (class.name.as_str(), class.invocable, class.registered))
            .collect::<Vec<_>>();
        assert_eq!(names, vec![("Hello", true, true), ("Util", false, false)]);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("scripts/Broken.js"), "{report:?}");
    }

    #[test]
    fn invoke_requires_trust_and_runs_async_scripts() {
        let _trust = IsolatedTrust::new();
        let (_dir, paths) = vault(
            "[customjs]\njs_files = [\"Job.js\"]\nstartup_scripts = [\"Job\"]\n",
            &[(
                "Job.js",
                "class Job { async invoke() { console.log('ran'); return 42; } }",
            )],
        );
        let error = invoke_customjs(&paths, None, "Job", false).expect_err("untrusted");
        assert!(error.to_string().contains("trusted vault"), "{error}");

        crate::trust::add_trust(paths.vault_root()).expect("trust");
        let invocation = invoke_customjs(&paths, None, "Job", false).expect("invoke");
        assert_eq!(invocation.result.value, Some(serde_json::json!(42)));
        let startup = run_customjs_startup(&paths, None, false).expect("startup");
        assert_eq!(startup.len(), 1);

        let missing = invoke_customjs(&paths, None, "Nope", false).expect_err("missing");
        assert!(missing.to_string().contains("is not defined"), "{missing}");
    }

    #[test]
    fn a_rejected_invoke_is_an_error() {
        let _trust = IsolatedTrust::new();
        let (_dir, paths) = vault(
            "[customjs]\njs_files = [\"Fail.js\"]\n",
            &[(
                "Fail.js",
                "class Fail { async invoke() { throw new Error('boom'); } }",
            )],
        );
        crate::trust::add_trust(paths.vault_root()).expect("trust");
        let error = invoke_customjs(&paths, None, "Fail", false).expect_err("rejects");
        assert!(error.to_string().contains("boom"), "{error}");
    }
}
