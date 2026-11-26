use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Duration;

use crate::assistant::{AssistantTool, AssistantToolSummary};
use crate::config::JsRuntimeSandbox;
use crate::dql::DqlQueryResult;
use crate::ResolvedPermissionProfile;
#[cfg(not(feature = "js_runtime"))]
use crate::VaultPaths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataviewJsError {
    Disabled,
    Message(String),
}

impl std::fmt::Display for DataviewJsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => formatter.write_str(
                "DataviewJS blocks require the `js_runtime` feature flag or an enabled [dataview] config",
            ),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DataviewJsError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataviewJsResult {
    pub outputs: Vec<DataviewJsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataviewJsToolDescriptor {
    #[serde(flatten)]
    pub summary: AssistantToolSummary,
    pub callable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataviewJsToolDefinition {
    #[serde(flatten)]
    pub tool: AssistantTool,
    pub callable: bool,
}

pub trait DataviewJsToolRegistry: Send + Sync {
    fn list(&self) -> Result<Vec<DataviewJsToolDescriptor>, String>;
    fn get(&self, name: &str) -> Result<DataviewJsToolDefinition, String>;
    fn call(&self, name: &str, input: &Value, options: Option<&Value>) -> Result<Value, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataviewJsMutationChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Application-layer commit boundary for script mutations.
///
/// Returning `true` means the committer persisted the complete change set.
/// Returning `false` asks the core runtime to preserve its ordinary Markdown
/// behavior after restoring the staged originals.
pub trait DataviewJsMutationCommitter: Send + Sync {
    fn commit(&self, changes: &[DataviewJsMutationChange]) -> Result<bool, String>;

    /// Move a note and rewrite the links to it, for hosts whose moves must
    /// go through their own write boundary. `None` uses the runtime's plain
    /// move.
    fn move_note(
        &self,
        _source: &str,
        _destination: &str,
    ) -> Option<Result<crate::MoveSummary, String>> {
        None
    }
}

#[derive(Clone, Default)]
pub struct DataviewJsEvalOptions {
    pub timeout: Option<Duration>,
    pub sandbox: Option<JsRuntimeSandbox>,
    pub permission_profile: Option<String>,
    pub resolved_permissions: Option<ResolvedPermissionProfile>,
    pub deterministic_static: bool,
    /// Deny filesystem writes, refactors, and host or shell execution even
    /// when the sandbox tier and profile allow them. Renders that only show
    /// a note set this so viewing never changes the vault.
    pub read_only: bool,
    pub disable_policy_hooks: bool,
    pub tool_registry: Option<Arc<dyn DataviewJsToolRegistry>>,
    pub mutation_committer: Option<Arc<dyn DataviewJsMutationCommitter>>,
    /// Exact argument vectors `host.exec()` may run from any sandbox tier
    /// without the `execute` permission: the host commands a device granted
    /// to the skill command being run (`vulcan tool grant`).
    pub granted_host_commands: Vec<Vec<String>>,
}

impl std::fmt::Debug for DataviewJsEvalOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataviewJsEvalOptions")
            .field("timeout", &self.timeout)
            .field("sandbox", &self.sandbox)
            .field("permission_profile", &self.permission_profile)
            .field("resolved_permissions", &self.resolved_permissions)
            .field("deterministic_static", &self.deterministic_static)
            .field("read_only", &self.read_only)
            .field("disable_policy_hooks", &self.disable_policy_hooks)
            .field("granted_host_commands", &self.granted_host_commands)
            .field(
                "tool_registry",
                &self.tool_registry.as_ref().map(|_| "<registered>"),
            )
            .field(
                "mutation_committer",
                &self.mutation_committer.as_ref().map(|_| "<registered>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataviewJsOutput {
    Query {
        result: DqlQueryResult,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    List {
        items: Vec<Value>,
    },
    TaskList {
        tasks: Vec<Value>,
        group_by_file: bool,
    },
    Paragraph {
        text: String,
    },
    Header {
        level: usize,
        text: String,
    },
    Element {
        element: String,
        text: String,
        attrs: Map<String, Value>,
    },
    Span {
        text: String,
    },
}

#[cfg(not(feature = "js_runtime"))]
pub fn evaluate_dataview_js(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
) -> Result<DataviewJsResult, DataviewJsError> {
    evaluate_dataview_js_with_options(
        paths,
        source,
        current_file,
        DataviewJsEvalOptions::default(),
    )
}

#[cfg(not(feature = "js_runtime"))]
pub fn evaluate_dataview_js_with_options(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
    options: DataviewJsEvalOptions,
) -> Result<DataviewJsResult, DataviewJsError> {
    let _ = (paths, source, current_file, options);
    Err(DataviewJsError::Disabled)
}

#[cfg(not(feature = "js_runtime"))]
pub fn evaluate_dataview_js_query(
    paths: &VaultPaths,
    source: &str,
    current_file: Option<&str>,
) -> Result<DataviewJsResult, DataviewJsError> {
    evaluate_dataview_js(paths, source, current_file)
}

#[cfg(not(feature = "js_runtime"))]
#[derive(Debug, Clone, Default)]
pub struct DataviewJsSession;

#[cfg(not(feature = "js_runtime"))]
impl DataviewJsSession {
    pub fn new(
        _paths: &VaultPaths,
        _current_file: Option<&str>,
        _options: DataviewJsEvalOptions,
    ) -> Result<Self, DataviewJsError> {
        Err(DataviewJsError::Disabled)
    }

    pub fn evaluate(&self, _source: &str) -> Result<DataviewJsResult, DataviewJsError> {
        Err(DataviewJsError::Disabled)
    }

    pub fn evaluate_sloppy(&self, _source: &str) -> Result<DataviewJsResult, DataviewJsError> {
        Err(DataviewJsError::Disabled)
    }

    pub fn evaluate_block(&self, _source: &str) -> Result<DataviewJsResult, DataviewJsError> {
        Err(DataviewJsError::Disabled)
    }

    pub fn evaluate_async_function(&self, _body: &str) -> Result<Option<Value>, DataviewJsError> {
        Err(DataviewJsError::Disabled)
    }
}

#[cfg(all(test, not(feature = "js_runtime")))]
mod disabled_tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::{scan_vault, ScanMode};

    use super::{evaluate_dataview_js_query, DataviewJsError};
    use crate::VaultPaths;

    #[test]
    fn dataviewjs_requires_runtime_feature() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should be created");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");

        let error = evaluate_dataview_js_query(&paths, "dv.current()", Some("Dashboard.md"))
            .expect_err("DataviewJS should be disabled without js_runtime");
        assert_eq!(error, DataviewJsError::Disabled);
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}

#[cfg(feature = "js_runtime")]
mod runtime {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::fs;
    use std::io::Read;
    use std::path::{Component, Path, PathBuf};
    use std::process::{Command as ProcessCommand, Stdio};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use csv::ReaderBuilder;
    use regex::Regex;
    use rquickjs::context::EvalOptions;
    use rquickjs::function::Func;
    use rquickjs::{
        CatchResultExt, CaughtError, Context, Ctx, Exception, Runtime, Value as JsValue,
    };
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    use super::{
        DataviewJsError, DataviewJsEvalOptions, DataviewJsMutationChange,
        DataviewJsMutationCommitter, DataviewJsOutput, DataviewJsResult, DataviewJsToolDescriptor,
        DataviewJsToolRegistry,
    };
    use crate::config::{
        load_vault_config, JsRuntimeConfig, JsRuntimeSandbox, VaultConfig, WebConfig,
    };
    use crate::dql::{evaluate_dql, DqlQueryResult};
    use crate::expression::eval::{compare_values, value_to_display};
    use crate::expression::functions::{
        format_date, format_duration, link_meta_value, parse_date_like_string,
        parse_duration_string,
    };
    use crate::file_metadata::FileMetadataResolver;
    use crate::graph::{
        query_graph_components_with_filter, query_graph_dead_ends_with_filter,
        query_graph_hubs_with_filter, query_graph_path_with_filter,
    };
    use crate::paths::secure_read_to_string;
    use crate::periodic::{
        list_daily_note_events, list_events_between, load_events_for_periodic_note,
        resolve_periodic_note, today_local_string,
    };
    use crate::permissions::{resolve_permission_profile, PermissionGuard, ProfilePermissionGuard};
    use crate::properties::NoteRecord;
    use crate::refactor::{
        plan_merge_tags, plan_rename_alias, plan_rename_block_ref, plan_rename_heading,
        plan_rename_property, set_note_property,
    };
    use crate::resolve_note_reference;
    use crate::search::{search_vault_with_filter, SearchQuery};
    #[cfg(feature = "web")]
    use crate::web::{
        fetch_web, prepare_search_backend, search_web, WebFetchReport, WebSearchReport,
    };
    // Without the `web` feature the bridge still exists, so scripts get a
    // clear error instead of a missing function.
    #[cfg(not(feature = "web"))]
    type WebSearchReport = serde_json::Value;
    #[cfg(not(feature = "web"))]
    type WebFetchReport = serde_json::Value;
    use crate::VaultPaths;
    use crate::{
        move_note, parse_document, query_backlinks_with_filter, query_links_with_filter,
        render_note_html, scan_vault, ScanMode,
    };
    use serde_json::{Map, Value};
    use std::cmp::Ordering;

    struct JsEvalState {
        paths: VaultPaths,
        current_file: Option<String>,
        note_index: Mutex<JsNoteIndex>,
        /// Universes replaced by reloads after mutations, kept so lazy page
        /// fields keep reading the snapshot their page came from.
        retired_note_indexes: Mutex<Vec<JsNoteIndex>>,
        periodic_config: crate::PeriodicConfig,
        inbox_config: crate::InboxConfig,
        #[cfg_attr(not(feature = "web"), allow(dead_code))]
        web_config: WebConfig,
        shell_path: Option<PathBuf>,
        sandbox: JsRuntimeSandbox,
        permissions: Option<ProfilePermissionGuard>,
        deterministic_static: bool,
        read_only: bool,
        link_resolution: crate::config::LinkResolutionMode,
        runtime_timeout: Option<Duration>,
        transaction: Mutex<Option<JsTransactionState>>,
        tool_registry: Option<Arc<dyn DataviewJsToolRegistry>>,
        mutation_committer: Option<Arc<dyn DataviewJsMutationCommitter>>,
        granted_host_commands: Vec<Vec<String>>,
    }

    /// The runtime's page universe (QRY.3): every readable note with stored
    /// fields and aliases, hydrated on demand where a page object or helper
    /// needs its file object (tags, links, inlinks, tasks, lists).
    struct JsNoteIndex {
        notes: HashMap<String, NoteRecord>,
        by_path: HashMap<String, String>,
        hydrated: HashSet<String>,
        complete: bool,
        /// Distinguishes this universe from those it replaced.
        generation: usize,
        /// Page selections returned with lazy file fields; the first lazy
        /// read in a selection hydrates all of it.
        page_sets: Vec<Vec<String>>,
        /// Link resolution over this universe, built on first use.
        resolver: Option<crate::resolver::ResolverIndex>,
    }

    /// What a runtime call reads from the page universe.
    enum IndexNeed {
        /// Stored fields only.
        Nothing,
        /// The file objects of these paths.
        Paths(Vec<String>),
        /// Every note's file object.
        All,
    }

    impl JsNoteIndex {
        fn new(notes: HashMap<String, NoteRecord>, generation: usize) -> Self {
            let by_path = notes
                .iter()
                .map(|(key, note)| (note.document_path.clone(), key.clone()))
                .collect();
            Self {
                notes,
                by_path,
                hydrated: HashSet::new(),
                complete: false,
                generation,
                page_sets: Vec::new(),
                resolver: None,
            }
        }

        fn note(&self, path: &str) -> Option<&NoteRecord> {
            self.by_path.get(path).and_then(|key| self.notes.get(key))
        }

        /// The path of the note a link `target` written in `origin` names,
        /// as Obsidian resolves it; an ambiguous target takes its first match.
        fn resolve_link_path(
            &mut self,
            target: &str,
            origin: Option<&str>,
            mode: crate::config::LinkResolutionMode,
        ) -> Option<String> {
            let notes = &self.notes;
            let resolver = self.resolver.get_or_insert_with(|| {
                let documents = notes
                    .values()
                    .map(|note| crate::resolver::ResolverDocument {
                        id: note.document_path.clone(),
                        path: note.document_path.clone(),
                        filename: note.file_name.clone(),
                        aliases: note.aliases.clone(),
                    })
                    .collect::<Vec<_>>();
                crate::resolver::ResolverIndex::build(&documents)
            });
            let origin = origin.unwrap_or_default();
            let result = resolver.resolve(
                &crate::resolver::ResolverLink {
                    source_document_id: origin.to_string(),
                    source_path: origin.to_string(),
                    target_path_candidate: Some(target.to_string()),
                    link_kind: crate::parser::LinkKind::Wikilink,
                },
                mode,
            );
            result.resolved_target_id.or_else(|| match result.problem {
                Some(crate::resolver::LinkResolutionProblem::Ambiguous(candidates)) => {
                    candidates.into_iter().next()
                }
                _ => None,
            })
        }

        fn hydrate(&mut self, state: &JsEvalState, need: IndexNeed) -> Result<(), DataviewJsError> {
            let wanted = match need {
                IndexNeed::Nothing => return Ok(()),
                IndexNeed::All if self.complete => return Ok(()),
                IndexNeed::All => {
                    self.complete = true;
                    self.by_path.keys().cloned().collect::<Vec<_>>()
                }
                IndexNeed::Paths(paths) => paths,
            };
            let missing = wanted
                .into_iter()
                .filter(|path| self.by_path.contains_key(path) && !self.hydrated.contains(path))
                .collect::<HashSet<_>>();
            if missing.is_empty() {
                return Ok(());
            }
            let scope = state.permissions.as_ref().map_or(
                crate::properties::NoteIndexReadScope::Filter(None),
                |guard| crate::properties::NoteIndexReadScope::Guard(guard),
            );
            crate::properties::hydrate_note_index_entries(
                &state.paths,
                scope,
                &mut self.notes,
                &missing,
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            self.hydrated.extend(missing);
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct JsTransactionState {
        originals: HashMap<String, Option<String>>,
    }

    const DEFAULT_HOST_TIMEOUT: Duration = Duration::from_secs(30);
    const DEFAULT_HOST_MAX_OUTPUT_BYTES: usize = 64 * 1024;
    const MAX_HOST_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

    #[derive(Debug, Clone, Serialize)]
    struct QueryResponse {
        successful: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<DqlQueryResult>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct HostCommandOptions {
        cwd: Option<String>,
        env: Option<Map<String, Value>>,
        timeout_ms: Option<u64>,
        max_output_bytes: Option<usize>,
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum HostInvocation {
        Exec { argv: Vec<String> },
        Shell { command: String, shell: String },
    }

    #[allow(clippy::struct_excessive_bools)]
    #[derive(Debug, Clone, Serialize)]
    struct HostProcessReport {
        invocation: HostInvocation,
        cwd: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        truncated_stdout: bool,
        truncated_stderr: bool,
        timed_out: bool,
        duration_ms: u128,
    }

    #[derive(Debug)]
    struct CapturedProcessOutput {
        text: String,
        truncated: bool,
    }

    pub struct DataviewJsSession {
        runtime: Runtime,
        context: Context,
        outputs: Arc<Mutex<Vec<DataviewJsOutput>>>,
        timeout: Option<Duration>,
    }

    const DATAVIEW_JS_PRELUDE: &str = r##"
const __vulcanDeterministicStatic = Boolean(globalThis.__vulcan_deterministic_static);

// Promises rejected with no handler yet; the host rethrows the first one
// once pending jobs have run.
const __vulcanUnhandledRejections = new Map();

function __vulcanTrackRejection(promise, reason, handled) {
  if (handled) {
    __vulcanUnhandledRejections.delete(promise);
  } else {
    __vulcanUnhandledRejections.set(promise, reason);
  }
}

// The completion value of an evaluation that ended in a promise.
let __vulcanResult;

function __vulcanAwaitResult(promise, wrapped) {
  __vulcanResult = { pending: true };
  promise.then(
    (value) => {
      __vulcanResult = { value: wrapped ? value?.value : value };
    },
    (error) => {
      __vulcanResult = { error };
    }
  );
}

function __vulcanTakeResult() {
  const result = __vulcanResult;
  __vulcanResult = undefined;
  if (!result || result.pending) {
    return undefined;
  }
  if ("error" in result) {
    throw result.error;
  }
  return result.value;
}

function __vulcanThrowUnhandledRejection() {
  for (const reason of __vulcanUnhandledRejections.values()) {
    __vulcanUnhandledRejections.clear();
    throw reason;
  }
}

function __vulcanStaticTimeError(operation) {
  throw new Error(`DataviewJS static mode does not allow wall-clock time via ${operation}; pass an explicit date or timestamp instead.`);
}

const __vulcanOriginalDate = Date;
if (__vulcanDeterministicStatic) {
  globalThis.Date = new Proxy(__vulcanOriginalDate, {
    apply(target, thisArg, args) {
      if ((args?.length ?? 0) === 0) {
        __vulcanStaticTimeError("Date()");
      }
      return Reflect.apply(target, thisArg, args);
    },
    construct(target, args, newTarget) {
      if ((args?.length ?? 0) === 0) {
        __vulcanStaticTimeError("new Date()");
      }
      return Reflect.construct(target, args, newTarget);
    },
    get(target, prop, receiver) {
      if (prop === "now") {
        return () => __vulcanStaticTimeError("Date.now()");
      }
      const value = Reflect.get(target, prop, receiver);
      return typeof value === "function" ? value.bind(target) : value;
    }
  });
}

class DataArray {
  constructor(values = []) {
    this.values = Array.from(values ?? []);
    return new Proxy(this, {
      get(target, prop, receiver) {
        if (prop in target) {
          const value = Reflect.get(target, prop, receiver);
          return typeof value === "function" ? value.bind(target) : value;
        }
        if (prop === Symbol.iterator) {
          return target.values[Symbol.iterator].bind(target.values);
        }
        if (typeof prop === "string" && /^-?\d+$/.test(prop)) {
          return target.values[Number(prop)];
        }
        if (typeof prop === "string") {
          return new DataArray(target.values.flatMap((value) => __vulcanSwizzle(value, prop)));
        }
        return undefined;
      }
    });
  }

  get length() {
    return this.values.length;
  }

  // Methods follow Dataview's DataArray: callbacks get (value, index,
  // values), sorting and grouping use the default value comparator unless
  // one is passed.
  array() {
    return Array.from(this.values);
  }

  where(predicate) {
    return new DataArray(this.values.filter(predicate));
  }

  filter(predicate) {
    return this.where(predicate);
  }

  map(mapper) {
    return new DataArray(this.values.map(mapper));
  }

  flatMap(mapper) {
    return new DataArray(
      this.values.flatMap((value, index, values) => __vulcanAsArray(mapper(value, index, values)))
    );
  }

  mutate(mutator) {
    this.values.forEach(mutator);
    return this;
  }

  sort(key, direction = "asc", comparator = undefined) {
    const compare = comparator ?? __vulcanDefaultComparator;
    const descending = String(direction).toLowerCase().startsWith("desc");
    const keyed = this.values.map((value, index, values) => ({
      value,
      key: __vulcanResolveKey(value, key, index, values),
    }));
    keyed.sort((left, right) =>
      descending ? -compare(left.key, right.key) : compare(left.key, right.key)
    );
    return new DataArray(keyed.map((entry) => entry.value));
  }

  sortInPlace(key, direction = "asc", comparator = undefined) {
    this.values = this.sort(key, direction, comparator).values;
    return this;
  }

  sortBy(key, direction = "asc", comparator = undefined) {
    return this.sort(key, direction, comparator);
  }

  groupBy(key, comparator = undefined) {
    const compare = comparator ?? __vulcanDefaultComparator;
    const keyed = this.values
      .map((value, index, values) => ({ value, key: __vulcanResolveKey(value, key, index, values) }));
    keyed.sort((left, right) => compare(left.key, right.key));
    const groups = [];
    for (const entry of keyed) {
      const last = groups[groups.length - 1];
      if (last && compare(last.key, entry.key) === 0) {
        last.rows.push(entry.value);
      } else {
        groups.push({ key: entry.key, rows: [entry.value] });
      }
    }
    return new DataArray(groups.map((group) => ({ key: group.key, rows: new DataArray(group.rows) })));
  }

  groupIn(key, comparator = undefined) {
    const isGrouping = this.values.length > 0
      && this.values.every((value) => value && typeof value === "object" && "key" in value && "rows" in value);
    if (!isGrouping) {
      return this.groupBy(key, comparator);
    }
    return this.map((group) => ({
      key: group.key,
      rows: new DataArray(__vulcanAsArray(group.rows)).groupIn(key, comparator),
    }));
  }

  unique() {
    return this.distinct();
  }

  distinct(key = undefined, comparator = undefined) {
    const compare = comparator ?? __vulcanDefaultComparator;
    const kept = [];
    this.values.forEach((value, index, values) => {
      const candidate = __vulcanResolveKey(value, key, index, values);
      if (!kept.some((entry) => compare(entry.key, candidate) === 0)) {
        kept.push({ key: candidate, value });
      }
    });
    return new DataArray(kept.map((entry) => entry.value));
  }

  limit(count) {
    return new DataArray(this.values.slice(0, count));
  }

  slice(start, end) {
    return new DataArray(this.values.slice(start, end));
  }

  concat(other) {
    return new DataArray(this.values.concat(__vulcanAsArray(other)));
  }

  indexOf(element, fromIndex = 0) {
    return this.findIndex((value) => __vulcanDefaultComparator(value, element) === 0, fromIndex);
  }

  find(predicate) {
    const index = this.findIndex(predicate);
    return index === -1 ? undefined : this.values[index];
  }

  findIndex(predicate, fromIndex = 0) {
    for (let index = fromIndex ?? 0; index < this.values.length; index++) {
      if (predicate(this.values[index], index, this.values)) {
        return index;
      }
    }
    return -1;
  }

  includes(element) {
    return this.indexOf(element) !== -1;
  }

  join(separator = ", ") {
    return this.values.map(__vulcanRenderScalar).join(separator ?? ", ");
  }

  every(predicate) {
    return this.values.every(predicate);
  }

  some(predicate) {
    return this.values.some(predicate);
  }

  none(predicate) {
    return this.values.every((value, index, values) => !predicate(value, index, values));
  }

  first() {
    return this.values.length > 0 ? this.values[0] : undefined;
  }

  last() {
    return this.values.length > 0 ? this.values[this.values.length - 1] : undefined;
  }

  to(key) {
    return new DataArray(this.values.flatMap((value) => __vulcanSwizzle(value, key)));
  }

  into(key) {
    return new DataArray(
      this.values.map((value) => (value == null ? undefined : value[key])).filter((value) => value != null)
    );
  }

  // Dataview takes a field name; a function is also accepted.
  expand(key) {
    const expanded = [];
    const queue = Array.from(this.values);
    while (queue.length > 0) {
      const next = queue.pop();
      if (next == null) {
        continue;
      }
      const children = typeof key === "function" ? key(next) : next[key];
      if (children == null) {
        continue;
      }
      for (const child of __vulcanAsArray(children)) {
        queue.push(child);
      }
      expanded.push(next);
    }
    return new DataArray(expanded);
  }

  forEach(callback) {
    this.values.forEach(callback);
  }

  sum() {
    return this.values.reduce((total, value) => total + value, 0);
  }

  avg() {
    return this.sum() / this.values.length;
  }

  min() {
    return Math.min(...this.values);
  }

  max() {
    return Math.max(...this.values);
  }

  toString() {
    return "[" + this.values.join(", ") + "]";
  }

  toJSON() {
    return this.values;
  }
}

function __vulcanAsArray(value) {
  if (value instanceof DataArray) {
    return value.values;
  }
  if (Array.isArray(value)) {
    return value;
  }
  return value == null ? [] : [value];
}

function __vulcanSwizzle(value, property) {
  if (value == null) {
    return [];
  }
  const candidate = value[property];
  if (candidate === undefined) {
    return [];
  }
  return __vulcanAsArray(candidate);
}

function __vulcanResolveKey(value, key, index = undefined, values = undefined) {
  if (key == null) {
    return value;
  }
  if (typeof key === "function") {
    return key(value, index, values);
  }
  return value == null ? undefined : value[key];
}

function __vulcanNormalizeComparable(value) {
  // Dataview compares links by where they point, not how they display.
  if (value instanceof Link) {
    return `${value.embed ? "!" : ""}[[${value.obsidianLink()}]]`;
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  if (value instanceof VulcanDateTime) {
    return value.toISO();
  }
  if (value && typeof value === "object" && value.__vulcanDuration === true) {
    return value.millis;
  }
  if (value instanceof DataArray) {
    return value.array().map(__vulcanNormalizeComparable);
  }
  if (Array.isArray(value)) {
    return value.map(__vulcanNormalizeComparable);
  }
  if (value && typeof value === "object") {
    const normalized = {};
    for (const [key, child] of Object.entries(value)) {
      normalized[key] = __vulcanNormalizeComparable(child);
    }
    return normalized;
  }
  return value;
}

function __vulcanPlain(value) {
  if (typeof value === "function") {
    return "[function " + (value.name || "anonymous") + "]";
  }
  if (value instanceof Link || value instanceof __VulcanListPairWidget) {
    return value.markdown();
  }
  if (value instanceof DataArray) {
    return value.array().map(__vulcanPlain);
  }
  if (value instanceof VulcanDateTime) {
    return value.toISO();
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  if (value && typeof value === "object" && value.__vulcanDuration === true) {
    return { millis: value.millis, text: value.text };
  }
  if (Array.isArray(value)) {
    return value.map(__vulcanPlain);
  }
  if (value && typeof value === "object") {
    const normalized = {};
    for (const [key, child] of Object.entries(value)) {
      normalized[key] = __vulcanPlain(child);
    }
    return normalized;
  }
  return value;
}

// Dataview's deepCopy: Links, dates, and durations stay what they are.
function __vulcanClone(value) {
  if (value instanceof Link) {
    return new Link({ ...value });
  }
  if (value instanceof DataArray) {
    return new DataArray(value.values.map(__vulcanClone));
  }
  if (Array.isArray(value)) {
    return value.map(__vulcanClone);
  }
  if (value && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    const copy = {};
    for (const [key, item] of Object.entries(value)) {
      copy[key] = __vulcanClone(item);
    }
    return copy;
  }
  return value;
}

function __vulcanSerialize(value) {
  return JSON.stringify(__vulcanPlain(value));
}

function __vulcanSerializeComparable(value) {
  const normalized = __vulcanNormalizeComparable(value);
  return JSON.stringify(normalized === undefined ? null : normalized);
}

// Dataview's default ordering of values (Values.compareValue).
function __vulcanDefaultComparator(left, right) {
  return __vulcan_compare(__vulcanSerializeComparable(left), __vulcanSerializeComparable(right));
}

function __vulcanCompareForSort(left, right, dir) {
  const ordering = __vulcan_compare(
    __vulcanSerializeComparable(left),
    __vulcanSerializeComparable(right)
  );
  return String(dir).toLowerCase().startsWith("desc") ? -ordering : ordering;
}

function __vulcanRenderScalar(value) {
  if (typeof value === "string") {
    return value;
  }
  if (value instanceof Link) {
    return value.markdown();
  }
  return JSON.stringify(__vulcanPlain(value));
}

function __vulcanMarkdownEscape(value) {
  return String(value).replace(/\|/g, "\\|");
}

function __vulcanWrapLike(template, values) {
  return template instanceof DataArray ? new DataArray(values) : values;
}

function __vulcanToNumber(value) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function __vulcanTruthy(value) {
  if (value instanceof DataArray) {
    return value.values.length > 0;
  }
  if (Array.isArray(value)) {
    return value.length > 0;
  }
  return !!value;
}

function __vulcanDateMillis(value) {
  if (value instanceof VulcanDateTime) {
    return value.toMillis();
  }
  if (value instanceof Date) {
    return value.getTime();
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const millis = __vulcan_date_millis(value);
    return millis == null ? null : millis;
  }
  return null;
}

function __vulcanReviveFileMetadata(file) {
  if (!file || typeof file !== "object" || Array.isArray(file)) {
    return file;
  }
  // Copy descriptors so lazy fields stay lazy.
  const revived = Object.defineProperties({}, Object.getOwnPropertyDescriptors(file));
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(revived))) {
    if ("value" in descriptor) {
      revived[key] = __vulcanReviveFileField(key, descriptor.value, file.path);
    }
  }
  // Dataview hands out every `file` array (tasks, lists, links, tags) as a
  // DataArray; lazy fields wrap theirs when they load.
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(revived))) {
    if (Array.isArray(descriptor.value)) {
      revived[key] = new DataArray(descriptor.value);
    }
  }
  for (const key of ["day", "mday", "cday", "mtime", "ctime"]) {
    if (typeof revived[key] === "string" || typeof revived[key] === "number") {
      const millis = __vulcanDateMillis(revived[key]);
      if (millis != null) {
        revived[key] = new VulcanDateTime(new Date(millis));
      }
    }
  }
  return revived;
}

const __vulcanLazyFileFields = ["etags", "inlinks", "lists", "outlinks", "tags", "tasks"];

// A file object whose hydrated fields load on first read: memoized,
// enumerable, and assignable like plain data, in serialization key order.
function __vulcanLazyFile(file, generation, set) {
  const path = file.path;
  const result = {};
  const keys = [...Object.keys(file), ...__vulcanLazyFileFields].sort();
  for (const key of keys) {
    if (!__vulcanLazyFileFields.includes(key)) {
      result[key] = file[key];
      continue;
    }
    const settle = (target, value) => {
      Object.defineProperty(target, key, {
        value,
        writable: true,
        enumerable: true,
        configurable: true,
      });
      return value;
    };
    Object.defineProperty(result, key, {
      enumerable: true,
      configurable: true,
      get() {
        const value = __vulcanReviveFileField(
          key,
          JSON.parse(__vulcan_page_file_field_json(generation, set, path, key)),
          path
        );
        return settle(this, Array.isArray(value) ? new DataArray(value) : value);
      },
      set(value) {
        settle(this, value);
      },
    });
  }
  return result;
}

function __vulcanLazyPages(source) {
  const { generation, set, pages } = JSON.parse(__vulcan_lazy_pages_json(source));
  return pages.map((page) =>
    page && page.file ? { ...page, file: __vulcanLazyFile(page.file, generation, set) } : page
  );
}

function __vulcanRevivePage(page) {
  if (!page || typeof page !== "object" || Array.isArray(page)) {
    return page;
  }
  const origin = page.file?.path ?? null;
  const revived = {};
  for (const [key, value] of Object.entries(page)) {
    revived[key] = key === "file" ? __vulcanReviveFileMetadata(value) : __vulcanReviveFieldValue(value, origin);
  }
  // Dataview's finalizeInlineFields: each field is also reachable by its
  // canonical name unless a field already has that name; fields sharing a
  // canonical name pool their values.
  const canonical = new Map();
  for (const [key, value] of Object.entries(revived)) {
    const name = __vulcanCanonicalizeVarName(key);
    if (key === "file" || name === "" || name in revived) {
      continue;
    }
    canonical.set(name, [...(canonical.get(name) ?? []), value]);
  }
  for (const [name, values] of canonical) {
    revived[name] = values.length === 1 ? values[0] : values;
  }
  return revived;
}

const __vulcanVarNamePart =
  /(\p{Extended_Pictographic}(?:\uFE0F|\p{Emoji_Modifier})?(?:\u200D\p{Extended_Pictographic}(?:\uFE0F|\p{Emoji_Modifier})?)*)|([0-9\p{L}_-]+)|(\s+)|[\s\S]/gu;

// Dataview's canonicalizeVarName: words lowercased, emoji kept, whitespace
// runs as `-`, and everything else dropped.
function __vulcanCanonicalizeVarName(name) {
  return String(name).replace(__vulcanVarNamePart, (_, emoji, word, space) => {
    if (emoji !== undefined) {
      return emoji;
    }
    if (word !== undefined) {
      return word.toLocaleLowerCase();
    }
    return space !== undefined ? "-" : "";
  });
}

function __vulcanDurationMillis(value) {
  if (value instanceof VulcanDuration) {
    return value.toMillis();
  }
  if (value && typeof value === "object" && value.__vulcanDuration === true) {
    return typeof value.millis === "number" ? value.millis : null;
  }
  if (value && typeof value === "object" && typeof value.millis === "number") {
    return value.millis;
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string") {
    const millis = __vulcan_duration_millis(value);
    return millis == null ? null : millis;
  }
  return null;
}

function __vulcanVectorizeUnary(value, mapper) {
  if (value instanceof DataArray) {
    return new DataArray(value.values.map((item) => __vulcanVectorizeUnary(item, mapper)));
  }
  if (Array.isArray(value)) {
    return value.map((item) => __vulcanVectorizeUnary(item, mapper));
  }
  return mapper(value);
}

function __vulcanVectorizeBinary(left, right, mapper, options = {}) {
  const { leftVector = true, rightVector = true } = options;
  if (leftVector && left instanceof DataArray) {
    return new DataArray(
      left.values.map((item) =>
        __vulcanVectorizeBinary(item, right, mapper, options)
      )
    );
  }
  if (leftVector && Array.isArray(left)) {
    return left.map((item) => __vulcanVectorizeBinary(item, right, mapper, options));
  }
  if (rightVector && right instanceof DataArray) {
    return new DataArray(
      right.values.map((item) =>
        __vulcanVectorizeBinary(left, item, mapper, options)
      )
    );
  }
  if (rightVector && Array.isArray(right)) {
    return right.map((item) => __vulcanVectorizeBinary(left, item, mapper, options));
  }
  return mapper(left, right);
}

function __vulcanVectorizeTernary(first, second, third, mapper, options = {}) {
  const {
    firstVector = true,
    secondVector = true,
    thirdVector = true,
  } = options;
  if (firstVector && first instanceof DataArray) {
    return new DataArray(
      first.values.map((item) =>
        __vulcanVectorizeTernary(item, second, third, mapper, options)
      )
    );
  }
  if (firstVector && Array.isArray(first)) {
    return first.map((item) =>
      __vulcanVectorizeTernary(item, second, third, mapper, options)
    );
  }
  if (secondVector && second instanceof DataArray) {
    return new DataArray(
      second.values.map((item) =>
        __vulcanVectorizeTernary(first, item, third, mapper, options)
      )
    );
  }
  if (secondVector && Array.isArray(second)) {
    return second.map((item) =>
      __vulcanVectorizeTernary(first, item, third, mapper, options)
    );
  }
  if (thirdVector && third instanceof DataArray) {
    return new DataArray(
      third.values.map((item) =>
        __vulcanVectorizeTernary(first, second, item, mapper, options)
      )
    );
  }
  if (thirdVector && Array.isArray(third)) {
    return third.map((item) =>
      __vulcanVectorizeTernary(first, second, item, mapper, options)
    );
  }
  return mapper(first, second, third);
}

function __vulcanEscapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function __vulcanRegex(value) {
  if (value instanceof RegExp) {
    return value;
  }
  if (typeof value !== "string") {
    return null;
  }
  const literal = value.match(/^\/(.*)\/([a-z]*)$/i);
  if (literal) {
    try {
      return new RegExp(literal[1], literal[2]);
    } catch {
      return null;
    }
  }
  try {
    return new RegExp(value);
  } catch {
    return null;
  }
}

function __vulcanTypeName(value) {
  if (value instanceof DataArray) {
    return "array";
  }
  if (value instanceof VulcanDuration) {
    return "duration";
  }
  if (value instanceof VulcanDateTime || value instanceof Date) {
    return "date";
  }
  if (value instanceof Link) {
    return "link";
  }
  if (typeof value === "string" && /^!?\[\[[^\]]+\]\]$/.test(value.trim())) {
    return "link";
  }
  if (value === null || value === undefined) {
    return "null";
  }
  if (Array.isArray(value)) {
    return "array";
  }
  if (typeof value === "boolean") {
    return "boolean";
  }
  if (typeof value === "number") {
    return "number";
  }
  if (typeof value === "string") {
    return "string";
  }
  if (typeof value === "object") {
    return "object";
  }
  return typeof value;
}

function __vulcanMeta(value) {
  if (value instanceof Link) {
    return {
      display: value.display ?? null,
      embed: value.embed,
      path: value.path,
      subpath: value.subpath ?? null,
      type: value.type,
    };
  }
  return JSON.parse(__vulcan_link_meta_json(__vulcanSerialize(value)));
}

function __vulcanContains(haystack, needle, { insensitive = false, exact = false } = {}) {
  if (haystack instanceof DataArray) {
    return haystack.values.some((item) =>
      __vulcanContains(item, needle, { insensitive, exact })
    );
  }
  if (Array.isArray(haystack)) {
    return haystack.some((item) =>
      exact
        ? dv.equal(item, needle)
        : __vulcanContains(item, needle, { insensitive, exact })
    );
  }
  if (haystack && typeof haystack === "object") {
    return Object.values(haystack).some((item) =>
      __vulcanContains(item, needle, { insensitive, exact })
    );
  }
  if (typeof haystack === "string") {
    const left = insensitive ? haystack.toLowerCase() : haystack;
    const rightText = __vulcanRenderScalar(needle);
    const right = insensitive ? rightText.toLowerCase() : rightText;
    return left.includes(right);
  }
  return dv.equal(haystack, needle);
}

function __vulcanExtreme(values, pickMax) {
  const items = values.flatMap(__vulcanAsArray);
  if (items.length === 0) {
    return null;
  }
  return items.reduce((best, item) => {
    if (best === null) {
      return item;
    }
    return (pickMax ? dv.compare(item, best) > 0 : dv.compare(item, best) < 0)
      ? item
      : best;
  }, null);
}

function __vulcanReduceOperator(left, right, operand) {
  const leftNumber = __vulcanToNumber(left);
  const rightNumber = __vulcanToNumber(right);
  switch (operand) {
    case "+":
      if (leftNumber != null && rightNumber != null) {
        return leftNumber + rightNumber;
      }
      return `${__vulcanRenderScalar(left)}${__vulcanRenderScalar(right)}`;
    case "-":
      return leftNumber == null || rightNumber == null ? null : leftNumber - rightNumber;
    case "*":
      return leftNumber == null || rightNumber == null ? null : leftNumber * rightNumber;
    case "/":
      return leftNumber == null || rightNumber == null || rightNumber === 0
        ? null
        : leftNumber / rightNumber;
    case "%":
      return leftNumber == null || rightNumber == null || rightNumber === 0
        ? null
        : leftNumber % rightNumber;
    default:
      return null;
  }
}

// Luxon's DateTime over a wall-clock time kept in UTC.
class VulcanDateTime {
  constructor(date) {
    this._date = new Date(date.getTime());
  }

  static now() {
    if (__vulcanDeterministicStatic) {
      __vulcanStaticTimeError("VulcanDateTime.now()");
    }
    return new VulcanDateTime(new Date());
  }

  static local(...parts) {
    if (parts.length === 0) {
      return VulcanDateTime.now();
    }
    const [year, month = 1, day = 1, hour = 0, minute = 0, second = 0, millisecond = 0] = parts;
    return VulcanDateTime.fromObject({ year, month, day, hour, minute, second, millisecond });
  }

  static utc(...parts) {
    return VulcanDateTime.local(...parts);
  }

  static fromISO(value) {
    const millis = __vulcan_date_millis(String(value));
    return millis == null ? null : new VulcanDateTime(new Date(millis));
  }

  static fromMillis(value) {
    return new VulcanDateTime(new Date(Number(value)));
  }

  static fromSeconds(value) {
    return VulcanDateTime.fromMillis(Number(value) * 1000);
  }

  static fromJSDate(value) {
    return new VulcanDateTime(value instanceof Date ? value : new Date(Number(value)));
  }

  static fromObject(value = {}) {
    const date = new Date(
      Date.UTC(
        Number(value.year ?? 1970),
        Number((value.month ?? 1) - 1),
        Number(value.day ?? 1),
        Number(value.hour ?? 0),
        Number(value.minute ?? 0),
        Number(value.second ?? 0),
        Number(value.millisecond ?? value.milliseconds ?? 0)
      )
    );
    return new VulcanDateTime(date);
  }

  static isDateTime(value) {
    return value instanceof VulcanDateTime;
  }

  static max(...dates) {
    return dates.reduce((best, date) => (best === undefined || date > best ? date : best), undefined);
  }

  static min(...dates) {
    return dates.reduce((best, date) => (best === undefined || date < best ? date : best), undefined);
  }

  get isValid() {
    return !Number.isNaN(this._date.getTime());
  }

  toISO() {
    return this._date.toISOString();
  }

  toISODate() {
    return this.toFormat("yyyy-MM-dd");
  }

  toISOTime() {
    return this._date.toISOString().slice(11);
  }

  toString() {
    return this.toISO();
  }

  toJSON() {
    return this.toISO();
  }

  valueOf() {
    return this.toMillis();
  }

  toMillis() {
    return this._date.getTime();
  }

  toSeconds() {
    return this.toMillis() / 1000;
  }

  toJSDate() {
    return new Date(this._date.getTime());
  }

  toObject() {
    return {
      year: this.year,
      month: this.month,
      day: this.day,
      hour: this.hour,
      minute: this.minute,
      second: this.second,
      millisecond: this.millisecond,
    };
  }

  toFormat(format) {
    return __vulcan_format_date(this.toMillis(), String(format));
  }

  toLocaleString() {
    return this.toFormat("M/d/yyyy");
  }

  toLocal() {
    return this;
  }

  toUTC() {
    return this;
  }

  setZone() {
    return this;
  }

  // Calendar units move the calendar (a month later is the same day of the
  // next month, clamped); the rest add their length.
  plus(value) {
    const duration = __vulcanToDuration(value);
    const date = this.toJSDate();
    const months = duration.years * 12 + duration.quarters * 3 + duration.months;
    if (months !== 0) {
      const day = date.getUTCDate();
      date.setUTCDate(1);
      date.setUTCMonth(date.getUTCMonth() + months);
      const lastDay = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0)).getUTCDate();
      date.setUTCDate(Math.min(day, lastDay));
    }
    const rest = duration.set({ years: 0, quarters: 0, months: 0 }).toMillis();
    return new VulcanDateTime(new Date(date.getTime() + rest));
  }

  minus(value) {
    return this.plus(__vulcanToDuration(value).negate());
  }

  diff(other, unit = "milliseconds") {
    const millis = this.toMillis() - __vulcanDateMillis(other);
    const units = (Array.isArray(unit) ? unit : [unit]).map(__vulcanDurationUnit);
    return VulcanDuration.fromMillis(millis).shiftTo(...units);
  }

  diffNow(unit = "milliseconds") {
    return this.diff(VulcanDateTime.now(), unit);
  }

  equals(other) {
    return other instanceof VulcanDateTime && this.toMillis() === other.toMillis();
  }

  hasSame(other, unit) {
    return this.startOf(unit).toMillis() === other.startOf(unit).toMillis();
  }

  startOf(unit) {
    const date = this.toJSDate();
    const normalized = String(unit).toLowerCase().replace(/s$/, "");
    if (normalized === "year") {
      date.setUTCMonth(0, 1);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "quarter") {
      date.setUTCMonth(Math.floor(date.getUTCMonth() / 3) * 3, 1);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "month") {
      date.setUTCDate(1);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "week") {
      const weekday = (date.getUTCDay() + 6) % 7;
      date.setUTCDate(date.getUTCDate() - weekday);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "day") {
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "hour") {
      date.setUTCMinutes(0, 0, 0);
    } else if (normalized === "minute") {
      date.setUTCSeconds(0, 0);
    } else if (normalized === "second") {
      date.setUTCMilliseconds(0);
    }
    return new VulcanDateTime(date);
  }

  endOf(unit) {
    const normalized = String(unit).toLowerCase().replace(/s$/, "");
    const step = { year: { years: 1 }, quarter: { months: 3 }, month: { months: 1 }, week: { weeks: 1 },
      day: { days: 1 }, hour: { hours: 1 }, minute: { minutes: 1 }, second: { seconds: 1 } }[normalized];
    if (step === undefined) {
      return this;
    }
    return this.startOf(unit).plus(step).minus({ milliseconds: 1 });
  }

  set(value = {}) {
    const date = this.toJSDate();
    if (value.year != null) date.setUTCFullYear(Number(value.year));
    if (value.month != null) date.setUTCMonth(Number(value.month) - 1);
    if (value.day != null) date.setUTCDate(Number(value.day));
    if (value.hour != null) date.setUTCHours(Number(value.hour));
    if (value.minute != null) date.setUTCMinutes(Number(value.minute));
    if (value.second != null) date.setUTCSeconds(Number(value.second));
    if (value.millisecond != null || value.milliseconds != null) {
      date.setUTCMilliseconds(Number(value.millisecond ?? value.milliseconds));
    }
    return new VulcanDateTime(date);
  }

  get(unit) {
    return this[unit];
  }

  get year() {
    return this._date.getUTCFullYear();
  }

  get quarter() {
    return Math.floor(this._date.getUTCMonth() / 3) + 1;
  }

  get month() {
    return this._date.getUTCMonth() + 1;
  }

  get day() {
    return this._date.getUTCDate();
  }

  get hour() {
    return this._date.getUTCHours();
  }

  get minute() {
    return this._date.getUTCMinutes();
  }

  get second() {
    return this._date.getUTCSeconds();
  }

  get millisecond() {
    return this._date.getUTCMilliseconds();
  }

  get weekday() {
    return ((this._date.getUTCDay() + 6) % 7) + 1;
  }

  get weekNumber() {
    return Number(this.toFormat("W"));
  }

  get weekYear() {
    return Number(this.toFormat("kkkk"));
  }

  get ordinal() {
    return Number(this.toFormat("o"));
  }

  get daysInMonth() {
    return new Date(Date.UTC(this.year, this.month, 0)).getUTCDate();
  }

  get daysInYear() {
    return new Date(Date.UTC(this.year, 1, 29)).getUTCMonth() === 1 ? 366 : 365;
  }

  get monthLong() {
    return this.toFormat("MMMM");
  }

  get monthShort() {
    return this.toFormat("MMM");
  }

  get weekdayLong() {
    return this.toFormat("EEEE");
  }

  get weekdayShort() {
    return this.toFormat("EEE");
  }

  get zoneName() {
    return "UTC";
  }

  get offset() {
    return 0;
  }
}

// Dataview's date grammar, the whole string: YYYY-MM, then optionally -DD,
// THH, :mm, :ss, .SSS in turn, and a zone only after the seconds.
const __vulcanDatePattern =
  /^(\d{4})-(\d{2})(?:-(\d{2})(?:T(\d{2})(?::(\d{2})(?::(\d{2})(?:\.(\d{3}))?([+-]\d{1,2}(?::\d{2})?|Z|\[[0-9A-Za-z+,\-./]+\])?)?)?)?)?$/;

function __vulcanParseDataviewDate(text) {
  const match = typeof text === "string" ? __vulcanDatePattern.exec(text) : null;
  if (!match) {
    return null;
  }
  const [year, month, day = 1, hour = 0, minute = 0, second = 0, millisecond = 0] = match
    .slice(1, 8)
    .map((part) => (part === undefined ? undefined : Number(part)));
  const date = new Date(Date.UTC(year, month - 1, day, hour, minute, second, millisecond));
  // Luxon refuses out-of-range parts rather than rolling them over.
  if (
    date.getUTCFullYear() !== year ||
    date.getUTCMonth() !== month - 1 ||
    date.getUTCDate() !== day ||
    hour > 23 ||
    minute > 59 ||
    second > 59
  ) {
    return null;
  }
  return new VulcanDateTime(date);
}

// Luxon's units, largest first, and its casual conversion matrix (a month
// is 30 days, a year 365).
const __vulcanDurationUnits = [
  "years", "quarters", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds",
];
const __vulcanDurationMillisPer = {
  years: 365 * 86400000,
  quarters: 91 * 86400000,
  months: 30 * 86400000,
  weeks: 7 * 86400000,
  days: 86400000,
  hours: 3600000,
  minutes: 60000,
  seconds: 1000,
  milliseconds: 1,
};
const __vulcanDurationMatrix = {
  years: { quarters: 4, months: 12, weeks: 52 },
  quarters: { months: 3, weeks: 13 },
  months: { weeks: 4 },
};

function __vulcanDurationFactor(larger, smaller) {
  return (
    __vulcanDurationMatrix[larger]?.[smaller] ??
    __vulcanDurationMillisPer[larger] / __vulcanDurationMillisPer[smaller]
  );
}

function __vulcanDurationUnit(unit) {
  const name = String(unit);
  const plural = name.endsWith("s") ? name : `${name}s`;
  if (!__vulcanDurationUnits.includes(plural)) {
    throw new Error(`Invalid unit ${unit}`);
  }
  return plural;
}

// Luxon's Duration: an amount per unit, as given.
class VulcanDuration {
  constructor(values = {}) {
    this.values = values;
    this.isLuxonDuration = true;
  }

  static fromObject(object = {}) {
    const values = {};
    for (const [unit, amount] of Object.entries(object ?? {})) {
      if (amount != null) {
        values[__vulcanDurationUnit(unit)] = Number(amount);
      }
    }
    return new VulcanDuration(values);
  }

  static fromMillis(value) {
    return new VulcanDuration({ milliseconds: Number(value) || 0 });
  }

  static fromISO(text) {
    const match =
      /^(-)?P(?:(-?\d+(?:[.,]\d+)?)Y)?(?:(-?\d+(?:[.,]\d+)?)M)?(?:(-?\d+(?:[.,]\d+)?)W)?(?:(-?\d+(?:[.,]\d+)?)D)?(?:T(?:(-?\d+(?:[.,]\d+)?)H)?(?:(-?\d+(?:[.,]\d+)?)M)?(?:(-?\d+(?:[.,]\d+)?)S)?)?$/.exec(
        String(text)
      );
    if (!match || match.slice(2).every((part) => part === undefined)) {
      return null;
    }
    const sign = match[1] ? -1 : 1;
    const values = {};
    ["years", "months", "weeks", "days", "hours", "minutes", "seconds"].forEach((unit, offset) => {
      const part = match[offset + 2];
      if (part !== undefined) {
        values[unit] = sign * Number(part.replace(",", "."));
      }
    });
    if (values.seconds !== undefined && !Number.isInteger(values.seconds)) {
      const whole = Math.trunc(values.seconds);
      values.milliseconds = Math.round((values.seconds - whole) * 1000);
      values.seconds = whole;
    }
    return new VulcanDuration(values);
  }

  static isDuration(value) {
    return value instanceof VulcanDuration;
  }

  get(unit) {
    return this.values[__vulcanDurationUnit(unit)] ?? 0;
  }

  get years() { return this.get("years"); }
  get quarters() { return this.get("quarters"); }
  get months() { return this.get("months"); }
  get weeks() { return this.get("weeks"); }
  get days() { return this.get("days"); }
  get hours() { return this.get("hours"); }
  get minutes() { return this.get("minutes"); }
  get seconds() { return this.get("seconds"); }
  get milliseconds() { return this.get("milliseconds"); }
  get isValid() { return true; }

  toObject() {
    return { ...this.values };
  }

  toMillis() {
    return this.as("milliseconds");
  }

  valueOf() {
    return this.toMillis();
  }

  as(unit) {
    const target = __vulcanDurationUnit(unit);
    return Object.entries(this.values).reduce(
      (total, [name, amount]) =>
        total + (amount * __vulcanDurationMillisPer[name]) / __vulcanDurationMillisPer[target],
      0
    );
  }

  // Luxon's shiftTo: re-express the amount in `units`; larger units roll
  // down, and leftovers become a fraction of the smallest unit asked for.
  shiftTo(...requested) {
    const units = requested.map(__vulcanDurationUnit);
    if (units.length === 0) {
      return this;
    }
    const built = {};
    const accumulated = {};
    let lastUnit;
    for (const unit of __vulcanDurationUnits) {
      if (units.includes(unit)) {
        lastUnit = unit;
        let own = 0;
        for (const [larger, amount] of Object.entries(accumulated)) {
          own += __vulcanDurationFactor(larger, unit) * amount;
          accumulated[larger] = 0;
        }
        own += this.values[unit] ?? 0;
        const whole = Math.trunc(own);
        built[unit] = whole;
        accumulated[unit] = (own * 1000 - whole * 1000) / 1000;
      } else if (this.values[unit] !== undefined) {
        accumulated[unit] = this.values[unit];
      }
    }
    for (const [unit, amount] of Object.entries(accumulated)) {
      if (amount !== 0) {
        built[lastUnit] +=
          unit === lastUnit ? amount : amount / __vulcanDurationFactor(lastUnit, unit);
      }
    }
    return new VulcanDuration(built).normalize();
  }

  shiftToAll() {
    return this.shiftTo(
      "years", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds"
    );
  }

  // Carry whole overflow of each unit into the next larger unit present.
  normalize() {
    const values = { ...this.values };
    const present = __vulcanDurationUnits.filter((unit) => values[unit] !== undefined);
    for (let index = present.length - 1; index > 0; index -= 1) {
      const smaller = present[index];
      const larger = present[index - 1];
      const factor = __vulcanDurationFactor(larger, smaller);
      const carry = Math.trunc(values[smaller] / factor);
      if (carry !== 0) {
        values[larger] += carry;
        values[smaller] -= carry * factor;
      }
    }
    return new VulcanDuration(values);
  }

  plus(other) {
    const added = __vulcanToDuration(other);
    const values = { ...this.values };
    for (const [unit, amount] of Object.entries(added.values)) {
      values[unit] = (values[unit] ?? 0) + amount;
    }
    return new VulcanDuration(values);
  }

  minus(other) {
    return this.plus(__vulcanToDuration(other).negate());
  }

  negate() {
    const values = {};
    for (const [unit, amount] of Object.entries(this.values)) {
      values[unit] = amount === 0 ? 0 : -amount;
    }
    return new VulcanDuration(values);
  }

  mapUnits(mapper) {
    const values = {};
    for (const [unit, amount] of Object.entries(this.values)) {
      values[unit] = mapper(amount, unit.slice(0, -1));
    }
    return new VulcanDuration(values);
  }

  set(values) {
    return new VulcanDuration({ ...this.values, ...VulcanDuration.fromObject(values).values });
  }

  equals(other) {
    return other instanceof VulcanDuration && this.toMillis() === other.toMillis();
  }

  // Luxon's toFormat: the duration shifted to the units the format names.
  toFormat(format) {
    const tokenUnits = {
      S: "milliseconds", s: "seconds", m: "minutes", h: "hours", d: "days",
      w: "weeks", M: "months", y: "years",
    };
    const tokens = [];
    const chars = [...String(format)];
    for (let index = 0; index < chars.length; ) {
      if (chars[index] === "'") {
        let literal = "";
        index += 1;
        while (index < chars.length && chars[index] !== "'") {
          literal += chars[index];
          index += 1;
        }
        index += 1;
        tokens.push({ literal: true, text: literal });
        continue;
      }
      let count = 1;
      while (chars[index + count] === chars[index]) {
        count += 1;
      }
      tokens.push({ literal: false, text: chars[index].repeat(count) });
      index += count;
    }
    const units = tokens
      .filter((token) => !token.literal && tokenUnits[token.text[0]])
      .map((token) => tokenUnits[token.text[0]]);
    const shifted = this.shiftTo(...units);
    return tokens
      .map((token) => {
        const unit = token.literal ? undefined : tokenUnits[token.text[0]];
        if (!unit) {
          return token.text;
        }
        const amount = Math.trunc(shifted.get(unit));
        const digits = String(Math.abs(amount)).padStart(token.text.length, "0");
        return amount < 0 ? `-${digits}` : digits;
      })
      .join("");
  }

  toISO() {
    const values = this.values;
    let date = "";
    let time = "";
    if (values.years) date += `${values.years}Y`;
    if (values.months || values.quarters) date += `${(values.months ?? 0) + (values.quarters ?? 0) * 3}M`;
    if (values.weeks) date += `${values.weeks}W`;
    if (values.days) date += `${values.days}D`;
    if (values.hours) time += `${values.hours}H`;
    if (values.minutes) time += `${values.minutes}M`;
    if (values.seconds || values.milliseconds) {
      time += `${Math.round(((values.seconds ?? 0) + (values.milliseconds ?? 0) / 1000) * 1000) / 1000}S`;
    }
    if (date === "" && time === "") {
      return "PT0S";
    }
    return `P${date}${time === "" ? "" : `T${time}`}`;
  }

  toJSON() {
    return this.toISO();
  }

  toString() {
    return this.toISO();
  }

  // Luxon's toHuman, in English: each unit with its amount.
  toHuman() {
    return Object.entries(this.values)
      .map(([unit, amount]) => `${amount} ${Math.abs(amount) === 1 ? unit.slice(0, -1) : unit}`)
      .join(", ");
  }

  // Vulcan's interchange shape for the Rust evaluator.
  get __vulcanDuration() {
    return true;
  }

  get millis() {
    return this.toMillis();
  }

  get text() {
    return __vulcanMinimalDuration(this);
  }
}

// Dataview's renderMinimalDuration: the normalized nonzero units, in words.
function __vulcanMinimalDuration(duration) {
  const values = Object.fromEntries(
    Object.entries(duration.shiftToAll().values).filter(([, amount]) => amount !== 0)
  );
  return new VulcanDuration(values).toHuman();
}

function __vulcanToDuration(value) {
  if (value instanceof VulcanDuration) {
    return value;
  }
  if (typeof value === "number") {
    return VulcanDuration.fromMillis(value);
  }
  if (value && typeof value === "object") {
    if (value.__vulcanDuration === true && typeof value.millis === "number") {
      return VulcanDuration.fromMillis(value.millis);
    }
    return VulcanDuration.fromObject(value);
  }
  if (typeof value === "string") {
    const text = value.trim();
    const parsed = __vulcanParseDuration(text) ?? VulcanDuration.fromISO(text);
    if (parsed !== null) {
      return parsed;
    }
    const millis = __vulcan_duration_millis(text);
    return millis == null ? new VulcanDuration() : VulcanDuration.fromMillis(millis);
  }
  return new VulcanDuration();
}

const __vulcanDurationTypes = {
  year: "years", years: "years", yr: "years", yrs: "years",
  month: "months", months: "months", mo: "months", mos: "months",
  week: "weeks", weeks: "weeks", wk: "weeks", wks: "weeks", w: "weeks",
  day: "days", days: "days", d: "days",
  hour: "hours", hours: "hours", hr: "hours", hrs: "hours", h: "hours",
  minute: "minutes", minutes: "minutes", min: "minutes", mins: "minutes", m: "minutes",
  second: "seconds", seconds: "seconds", sec: "seconds", secs: "seconds", s: "seconds",
};
const __vulcanDurationTypePattern = Object.keys(__vulcanDurationTypes)
  .sort((left, right) => right.length - left.length)
  .join("|");
const __vulcanDurationPart = new RegExp(
  `(-?[0-9]+(?:\\.[0-9]+)?)\\s*(${__vulcanDurationTypePattern})`,
  "y"
);

// Dataview's duration grammar: `<number> <unit>` parts separated by commas
// or spaces, the whole string, summed unit by unit.
function __vulcanParseDuration(text) {
  if (typeof text !== "string") {
    return null;
  }
  const values = {};
  let index = 0;
  let parts = 0;
  while (index < text.length || parts === 0) {
    if (parts > 0) {
      const separator = /\s*,\s*|\s*/y;
      separator.lastIndex = index;
      separator.exec(text);
      index = separator.lastIndex;
    }
    __vulcanDurationPart.lastIndex = index;
    const match = __vulcanDurationPart.exec(text);
    if (!match) {
      return null;
    }
    const unit = __vulcanDurationTypes[match[2]];
    values[unit] = (values[unit] ?? 0) + Number(match[1]);
    index = __vulcanDurationPart.lastIndex;
    parts += 1;
  }
  return new VulcanDuration(values);
}

// Dataview's frontmatter typing (parseFrontmatter): a whole string that is a
// date, a duration, or a wikilink becomes one; dates in objects and arrays
// too. Links stay as written.
function __vulcanParseFrontmatter(value) {
  if (value == null) {
    return null;
  }
  if (typeof value === "string") {
    return (
      __vulcanParseDataviewDate(value) ??
      __vulcanParseDuration(value) ??
      __vulcanParseWikilink(value) ??
      value
    );
  }
  if (value instanceof Date) {
    return VulcanDateTime.fromJSDate(value);
  }
  if (Array.isArray(value)) {
    return value.map(__vulcanParseFrontmatter);
  }
  if (typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    const parsed = {};
    for (const [key, item] of Object.entries(value)) {
      parsed[key] = __vulcanParseFrontmatter(item);
    }
    return parsed;
  }
  return value;
}

const __vulcanInlineDatePart =
  /\d{4}-\d{2}(?:-\d{2}(?:T\d{2}(?::\d{2}(?::\d{2}(?:\.\d{3})?(?:[+-]\d{1,2}(?::\d{2})?|Z|\[[0-9A-Za-z+,\-./]+\])?)?)?)?)?/y;
const __vulcanInlineDurationPart = new RegExp(
  `-?[0-9]+(?:\\.[0-9]+)?\\s*(?:${__vulcanDurationTypePattern})(?:(?:\\s*,\\s*|\\s*)-?[0-9]+(?:\\.[0-9]+)?\\s*(?:${__vulcanDurationTypePattern}))*`,
  "y"
);
const __vulcanInlineStringPart = /"((?:\\.|[^"\\])*)"/y;
const __vulcanInlineTagPart =
  /#[^\u2000-\u206F\u2E00-\u2E7F'!"#$%&()*+,.:;<=>?@^`{|}~\[\]\\\s]*/y;
const __vulcanInlineLinkPart = /!?\[\[[^\[\]]*?\]\]/uy;
const __vulcanInlineBoolPart = /true|false|True|False/y;
const __vulcanInlineNumberPart = /-?[0-9]+(?:\.[0-9]+)?/y;
const __vulcanInlineNullPart = /null/y;

function __vulcanStickyMatch(pattern, text, index) {
  pattern.lastIndex = index;
  const match = pattern.exec(text);
  return match === null ? null : { match, end: pattern.lastIndex };
}

// One of Dataview's inline atoms at `index`: the first kind that matches
// wins, as in its grammar, and a date that is not a real date falls through.
function __vulcanInlineAtom(text, index) {
  const date = __vulcanStickyMatch(__vulcanInlineDatePart, text, index);
  if (date !== null) {
    const value = __vulcanParseDataviewDate(date.match[0]);
    if (value !== null) {
      return { value, end: date.end };
    }
  }
  const duration = __vulcanStickyMatch(__vulcanInlineDurationPart, text, index);
  if (duration !== null) {
    return { value: __vulcanParseDuration(duration.match[0]).shiftToAll().normalize(), end: duration.end };
  }
  const string = __vulcanStickyMatch(__vulcanInlineStringPart, text, index);
  if (string !== null) {
    const value = string.match[1].replace(/\\(.)/g, (_, escaped) =>
      escaped === '"' || escaped === "\\" ? escaped : `\\${escaped}`
    );
    return { value, end: string.end };
  }
  const tag = __vulcanStickyMatch(__vulcanInlineTagPart, text, index);
  if (tag !== null) {
    return { value: tag.match[0], end: tag.end };
  }
  const link = __vulcanStickyMatch(__vulcanInlineLinkPart, text, index);
  if (link !== null) {
    return { value: __vulcanParseWikilink(link.match[0]), end: link.end };
  }
  const bool = __vulcanStickyMatch(__vulcanInlineBoolPart, text, index);
  if (bool !== null) {
    return { value: bool.match[0].toLowerCase() === "true", end: bool.end };
  }
  const number = __vulcanStickyMatch(__vulcanInlineNumberPart, text, index);
  if (number !== null) {
    return { value: Number.parseFloat(number.match[0]), end: number.end };
  }
  const rawNull = __vulcanStickyMatch(__vulcanInlineNullPart, text, index);
  return rawNull === null ? null : { value: rawNull.match[0], end: rawNull.end };
}

// Dataview's inline value grammar (dv.parse): one atom, or atoms separated
// by commas as an array, covering the whole text; anything else stays text.
function __vulcanParseInlineValue(text) {
  if (typeof text !== "string") {
    return text;
  }
  const first = __vulcanInlineAtom(text, 0);
  if (first === null) {
    return text;
  }
  const values = [first.value];
  let index = first.end;
  const separator = /\s*,\s*/y;
  while (index < text.length) {
    const comma = __vulcanStickyMatch(separator, text, index);
    const next = comma === null ? null : __vulcanInlineAtom(text, comma.end);
    if (next === null) {
      return text;
    }
    values.push(next.value);
    index = next.end;
  }
  return values.length === 1 ? values[0] : values;
}

// Dataview's Link: a file, header, or block reference.
class Link {
  constructor(fields) {
    Object.assign(this, fields);
  }

  static file(path, embed = false, display = undefined) {
    return new Link({ path, embed, display, subpath: undefined, type: "file" });
  }

  static infer(linkpath, embed = false, display = undefined) {
    if (linkpath.includes("#^")) {
      const [path, blockId] = linkpath.split("#^");
      return Link.block(path, blockId, embed, display);
    }
    if (linkpath.includes("#")) {
      const [path, header] = linkpath.split("#");
      return Link.header(path, header, embed, display);
    }
    return Link.file(linkpath, embed, display);
  }

  static header(path, header, embed = false, display = undefined) {
    return new Link({
      path,
      embed,
      display,
      subpath: __vulcanNormalizeHeaderForLink(header),
      type: "header",
    });
  }

  static block(path, blockId, embed = false, display = undefined) {
    return new Link({ path, embed, display, subpath: blockId, type: "block" });
  }

  static fromObject(object) {
    return new Link(object);
  }

  equals(other) {
    return (
      other != null &&
      this.path == other.path &&
      this.type == other.type &&
      this.subpath == other.subpath
    );
  }

  toString() {
    return this.markdown();
  }

  toJSON() {
    return this.markdown();
  }

  toObject() {
    return {
      path: this.path,
      type: this.type,
      subpath: this.subpath,
      display: this.display,
      embed: this.embed,
    };
  }

  withPath(path) {
    return new Link({ ...this, path });
  }

  withDisplay(display) {
    return new Link({ ...this, display });
  }

  withHeader(header) {
    return Link.header(this.path, header, this.embed, this.display);
  }

  toFile() {
    return Link.file(this.path, this.embed, this.display);
  }

  toEmbed() {
    return this.embed ? this : new Link({ ...this, embed: true });
  }

  fromEmbed() {
    return this.embed ? new Link({ ...this, embed: false }) : this;
  }

  markdown() {
    let result = `${this.embed ? "!" : ""}[[${this.obsidianLink()}`;
    if (this.display) {
      result += `|${this.display}`;
    } else {
      result += `|${__vulcanFileTitle(this.path)}`;
      if (this.type == "header" || this.type == "block") {
        result += ` > ${this.subpath}`;
      }
    }
    return `${result}]]`;
  }

  obsidianLink() {
    const escaped = this.path.replaceAll("|", "\\|");
    if (this.type == "header") {
      return `${escaped}#${this.subpath?.replaceAll("|", "\\|")}`;
    }
    if (this.type == "block") {
      return `${escaped}#^${this.subpath?.replaceAll("|", "\\|")}`;
    }
    return escaped;
  }

  fileName() {
    return __vulcanFileTitle(this.path).replace(".md", "");
  }
}

function __vulcanFileTitle(path) {
  let title = String(path);
  if (title.includes("/")) {
    title = title.substring(title.lastIndexOf("/") + 1);
  }
  return title.endsWith(".md") ? title.substring(0, title.length - 3) : title;
}

// Dataview's header canonicalization: letters, digits, `_`, `-`, and emoji
// stay; anything else becomes a space, and runs of spaces collapse.
function __vulcanNormalizeHeaderForLink(header) {
  return String(header)
    .replace(/[^\p{L}\p{N}_\-\p{Extended_Pictographic}‍️]/gu, " ")
    .split(/\s+/)
    .join(" ")
    .trim();
}

const __vulcanWikilinkPattern = /^(!?)\[\[([^\[\]]*?)\]\]$/u;

// The Link a whole-string `[[wikilink]]` names, unresolved; null otherwise.
function __vulcanParseWikilink(text) {
  const match = typeof text === "string" ? __vulcanWikilinkPattern.exec(text) : null;
  if (!match) {
    return null;
  }
  let inner = match[2];
  let display;
  let pipe = -1;
  while ((pipe = inner.indexOf("|", pipe + 1)) >= 0) {
    if (pipe > 0 && inner[pipe - 1] == "\\") {
      continue;
    }
    display = inner.substring(pipe + 1);
    inner = inner.substring(0, pipe);
    break;
  }
  const link = Link.infer(inner.replace(/\\\|/g, "|"), false, display);
  link.embed = match[1] === "!";
  return link;
}

// The path a link `target` written in `origin` names; the target itself when
// no note matches, and `origin` for a link within the same note.
function __vulcanResolveLinkPath(target, origin) {
  if (!target) {
    return origin ?? target;
  }
  return __vulcan_resolve_link_path(String(target), origin ?? null) ?? target;
}

// A whole-string `[[wikilink]]` written in `origin` as a resolved Link.
function __vulcanResolvedLink(text, origin) {
  const link = __vulcanParseWikilink(text);
  return link === null ? null : link.withPath(__vulcanResolveLinkPath(link.path, origin));
}

// Field values as Dataview types them: whole-string dates, durations, and
// wikilinks, at any depth, become DateTimes, Durations, and resolved Links.
function __vulcanReviveFieldValue(value, origin) {
  if (typeof value === "string") {
    const parsed = __vulcanParseFrontmatter(value);
    return parsed instanceof Link
      ? parsed.withPath(__vulcanResolveLinkPath(parsed.path, origin))
      : parsed;
  }
  if (Array.isArray(value)) {
    return value.map((item) => __vulcanReviveFieldValue(item, origin));
  }
  if (value && typeof value === "object" && Object.getPrototypeOf(value) === Object.prototype) {
    const revived = {};
    for (const [key, item] of Object.entries(value)) {
      revived[key] = __vulcanReviveFieldValue(item, origin);
    }
    return revived;
  }
  return value;
}

// A link Obsidian found in a note, with the text it shows as its display.
function __vulcanOutlink(text, origin) {
  const link = __vulcanResolvedLink(text, origin);
  if (link === null || link.display !== undefined) {
    return link ?? text;
  }
  const inner = __vulcanWikilinkPattern.exec(text)[2];
  return link.withDisplay(inner);
}

const __vulcanListItemPlainKeys = new Set([
  "text", "visual", "symbol", "path", "status", "statusName", "statusType", "statusNext",
  "taskSource", "blockId", "tags", "line", "lineCount", "parent", "list", "position",
]);

function __vulcanReviveListItem(item, origin) {
  if (!item || typeof item !== "object") {
    return item;
  }
  const revived = {};
  for (const [key, value] of Object.entries(item)) {
    if (key === "link" || key === "section" || key === "header") {
      revived[key] = typeof value === "string" ? __vulcanResolvedLink(value, origin) ?? value : value;
    } else if (key === "outlinks") {
      revived[key] = __vulcanAsArray(value).map((link) => __vulcanOutlink(link, origin));
    } else if (key === "children" || key === "subtasks") {
      revived[key] = __vulcanAsArray(value).map((child) => __vulcanReviveListItem(child, origin));
    } else if (__vulcanListItemPlainKeys.has(key)) {
      revived[key] = value;
    } else {
      revived[key] = __vulcanReviveFieldValue(value, origin);
    }
  }
  // Dataview's deprecated aliases, kept for older scripts.
  if ("children" in revived && !("subtasks" in revived)) {
    revived.subtasks = revived.children;
  }
  if ("task" in revived && !("real" in revived)) {
    revived.real = revived.task;
  }
  if ("section" in revived && !("header" in revived)) {
    revived.header = revived.section;
  }
  return revived;
}

// A `file` field as Dataview types it.
function __vulcanReviveFileField(key, value, path) {
  switch (key) {
    case "link":
      return typeof value === "string" ? Link.file(path) : value;
    case "outlinks":
      return __vulcanAsArray(value).map((link) => __vulcanOutlink(link, path));
    case "inlinks":
      return __vulcanAsArray(value).map((link) => {
        const resolved = __vulcanResolvedLink(link, path);
        return resolved === null ? link : Link.file(resolved.path);
      });
    case "lists":
    case "tasks":
      return __vulcanAsArray(value).map((item) => __vulcanReviveListItem(item, path));
    default:
      return value;
  }
}

function __vulcanQuerySync(dql, file) {
  return JSON.parse(__vulcan_query_json(dql, file));
}

function __vulcanTryQuerySync(dql, file) {
  const result = __vulcanQuerySync(dql, file);
  if (!result.successful) {
    throw new Error(result.error ?? "Dataview query failed");
  }
  return result.value;
}

const __vulcanDataviewSettings = {
  renderNullAs: "\\-",
  defaultDateFormat: "MMMM dd, yyyy",
  defaultDateTimeFormat: "h:mm a - MMMM dd, yyyy",
  ...JSON.parse(globalThis.__vulcan_dataview_settings_json ?? "{}"),
};

// Dataview's Values.toString: how a value reads in rendered Markdown.
function __vulcanValueToString(value, recursive = false) {
  if (value == null) {
    return __vulcanDataviewSettings.renderNullAs;
  }
  if (value instanceof __VulcanListPairWidget || value instanceof Link) {
    return value.markdown();
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (typeof value === "function") {
    return "<function>";
  }
  if (value instanceof VulcanDateTime) {
    const dateOnly = value.hour === 0 && value.minute === 0 && value.second === 0;
    return value.toFormat(
      dateOnly
        ? __vulcanDataviewSettings.defaultDateFormat
        : __vulcanDataviewSettings.defaultDateTimeFormat
    );
  }
  if (value instanceof VulcanDuration || value.__vulcanDuration === true) {
    return String(value);
  }
  if (value instanceof DataArray || Array.isArray(value)) {
    const inner = __vulcanAsArray(value)
      .map((item) => __vulcanValueToString(item, true))
      .join(", ");
    return recursive ? `[${inner}]` : inner;
  }
  if (typeof value === "object") {
    const entries = Object.entries(value).map(
      ([key, item]) => `${key}: ${__vulcanValueToString(item, true)}`
    );
    return `{ ${entries.join(", ")} }`;
  }
  return String(value);
}

// Dataview's markdownTable: padded columns, with arrays and objects as HTML lists.
function __vulcanMarkdownTable(headers, rows) {
  if (rows.length > 0 && headers.length !== rows[0].length) {
    throw new Error(
      `The number of headers (${headers.length}) must match the number of columns (${rows[0].length})`
    );
  }
  const escape = (text) => String(text).split(/(?!\\)\|/i).join("\\|");
  const literal = (value) => {
    if (value instanceof DataArray || Array.isArray(value)) {
      return `<ul>${__vulcanAsArray(value).map((item) => `<li>${escape(literal(item))}</li>`).join("")}</ul>`;
    }
    if (
      value &&
      typeof value === "object" &&
      !(value instanceof VulcanDateTime) &&
      !(value instanceof VulcanDuration) &&
      !(value instanceof __VulcanListPairWidget) &&
      !(value instanceof Link) &&
      value.__vulcanDuration !== true
    ) {
      const inner = Object.entries(value)
        .map(([key, item]) => `<li><b>${escape(key)}</b>: ${escape(literal(item))}</li>`)
        .join("");
      return `<ul>${inner}</ul>`;
    }
    return __vulcanValueToString(value);
  };
  const pad = (text, padding, length) =>
    text.length >= length ? text : text + padding.repeat(length - text.length);
  const widths = headers.map((header) => escape(header).length);
  const cells = rows.map((row) =>
    row.map((value, column) => {
      const text = escape(literal(value));
      widths[column] = Math.max(widths[column] ?? 0, text.length);
      return text;
    })
  );
  let table = `| ${headers.map((header, column) => pad(escape(header), " ", widths[column])).join(" | ")} |\n`;
  table += `| ${widths.map((width) => pad("", "-", width)).join(" | ")} |\n`;
  for (const row of cells) {
    table += `| ${row.map((text, column) => pad(text, " ", widths[column])).join(" | ")} |\n`;
  }
  return table;
}

// Dataview's markdownList: nested arrays and objects indent by four spaces.
function __vulcanMarkdownList(input, depth) {
  if (input instanceof DataArray || Array.isArray(input)) {
    let result = depth === 0 ? "" : "\n";
    for (const value of __vulcanAsArray(input)) {
      result += `${"    ".repeat(depth)}- ${__vulcanMarkdownList(value, depth)}\n`;
    }
    return result;
  }
  if (
    input &&
    typeof input === "object" &&
    !(input instanceof VulcanDateTime) &&
    !(input instanceof VulcanDuration) &&
    !(input instanceof __VulcanListPairWidget) &&
    !(input instanceof Link) &&
    input.__vulcanDuration !== true
  ) {
    let result = depth === 0 ? "" : "\n";
    for (const [key, value] of Object.entries(input)) {
      result += `${"    ".repeat(depth)}- ${key}: ${__vulcanMarkdownList(value, depth)}\n`;
    }
    return result;
  }
  return __vulcanValueToString(input);
}

// Dataview's markdownTaskList: groups become headings, and items listed under
// a parent that is also present render only beneath it.
function __vulcanMarkdownTaskList(items, depth) {
  if (items.length > 0 && items.every((item) => item && "key" in item && "rows" in item)) {
    let result = "";
    for (const group of items) {
      result += `${"#".repeat(depth + 1)} ${__vulcanValueToString(group.key)}\n\n`;
      result += __vulcanMarkdownTaskList(__vulcanAsArray(group.rows), depth + 1);
    }
    return result;
  }
  const lineKey = (item, line) => `${item.path ?? ""}:${line}`;
  const present = new Set(items.map((item) => lineKey(item, item.line)));
  let result = "";
  for (const item of items) {
    if (item.parent != null && present.has(lineKey(item, item.parent))) {
      continue;
    }
    const text = String(item.visual ?? item.text ?? "").split("\n").join(" ");
    result += `${"    ".repeat(depth)}- ${item.task ? `[${item.status ?? " "}] ` : ""}${text}\n`;
    const children = __vulcanAsArray(item.children).filter(
      (child) => child && typeof child === "object"
    );
    result += __vulcanMarkdownTaskList(children, depth + 1);
  }
  return result;
}

// Dataview's list-pair widget: a LIST row's id and its value.
class __VulcanListPairWidget {
  constructor(key, value) {
    this.$widget = "dataview:list-pair";
    this.key = key;
    this.value = value;
  }

  markdown() {
    return `${__vulcanValueToString(this.key)}: ${__vulcanValueToString(this.value)}`;
  }

  toString() {
    return this.markdown();
  }
}

// Dataview's QueryResult for a Vulcan DQL result: LIST values are ids,
// list pairs, or bare values; TABLE values are rows in header order; TASK
// values are the tasks as `file.tasks` shapes them. Values are typed as
// page fields are, with links resolved from `origin`.
function __vulcanDataviewQueryResult(raw, origin) {
  const columns = raw?.columns ?? [];
  const rows = raw?.rows ?? [];
  const revive = (value) => __vulcanReviveFieldValue(value, origin);
  const idMeaning = columns[0] === "Group" ? { type: "group", name: "Group" } : { type: "path" };
  switch (raw?.query_type) {
    case "list":
      return {
        type: "list",
        values: rows.map((row) =>
          columns.length >= 2
            ? new __VulcanListPairWidget(revive(row[columns[0]]), revive(row[columns[1]]))
            : revive(row[columns[0]])
        ),
        primaryMeaning: idMeaning,
      };
    case "table":
      return {
        type: "table",
        headers: [...columns],
        values: rows.map((row) => columns.map((column) => revive(row[column]))),
        idMeaning,
      };
    case "task": {
      const pages = new Map();
      const tasksOf = (path) => {
        if (!pages.has(path)) {
          pages.set(path, __vulcanAsArray(dv.page(path)?.file?.tasks ?? []));
        }
        return pages.get(path);
      };
      return {
        type: "task",
        values: rows.map(
          (row) =>
            (row.path ? tasksOf(row.path).find((task) => task.line === row.line - 1) : undefined) ??
            __vulcanReviveListItem(row, row.path ?? origin)
        ),
      };
    }
    default:
      return raw;
  }
}

// Dataview's queryMarkdown: the result through its Markdown helpers.
function __vulcanQueryResultMarkdown(result) {
  switch (result.type) {
    case "list":
      return __vulcanMarkdownList(result.values, 0);
    case "table":
      return __vulcanMarkdownTable(result.headers, result.values);
    case "task":
      return __vulcanMarkdownTaskList(result.values, 0);
    default:
      throw new Error(`Cannot render ${result.type} queries to markdown.`);
  }
}

function buildDvFunctions(dv) {
  return {
    date: dv.date,
    dur: dv.duration,
    duration: dv.duration,
    number(value) {
      return __vulcanVectorizeUnary(value, (item) => {
        if (item == null) {
          return null;
        }
        return __vulcanToNumber(item);
      });
    },
    string(value) {
      return __vulcanVectorizeUnary(value, (item) =>
        item == null ? null : __vulcanRenderScalar(item)
      );
    },
    // Dataview's link(): a Link to the note `path` resolves to.
    link(path, display) {
      return __vulcanVectorizeUnary(path, (item) => {
        if (item == null) {
          return null;
        }
        const link =
          item instanceof Link
            ? item
            : Link.file(__vulcanResolveLinkPath(String(item), dv.currentFilePath || null));
        return display == null ? link : link.withDisplay(String(display));
      });
    },
    embed(value, shouldEmbed = true) {
      if (value instanceof Link) {
        return shouldEmbed ? value.toEmbed() : value.fromEmbed();
      }
      if (typeof value !== "string") {
        return null;
      }
      const trimmed = value.trim();
      if (!/^!?\[\[[^\]]+\]\]$/.test(trimmed)) {
        return null;
      }
      return shouldEmbed
        ? trimmed.startsWith("!") ? trimmed : `!${trimmed}`
        : trimmed.startsWith("!") ? trimmed.slice(1) : trimmed;
    },
    elink(url, display) {
      if (url == null) {
        return null;
      }
      const target = String(url);
      return `[${display == null ? target : String(display)}](${target})`;
    },
    typeof(value) {
      return __vulcanVectorizeUnary(value, __vulcanTypeName);
    },
    object(...args) {
      const output = {};
      for (let index = 0; index < args.length; index += 2) {
        output[String(args[index])] = index + 1 < args.length ? args[index + 1] : null;
      }
      return output;
    },
    list(...args) {
      return args.length === 1 && (args[0] instanceof DataArray || Array.isArray(args[0]))
        ? __vulcanAsArray(args[0])
        : args;
    },
    round(value, digits = 0) {
      return __vulcanVectorizeBinary(
        value,
        digits,
        (item, precision) => {
          const number = __vulcanToNumber(item);
          const decimals = __vulcanToNumber(precision) ?? 0;
          if (number == null) {
            return null;
          }
          const factor = 10 ** decimals;
          return Math.round(number * factor) / factor;
        },
        { rightVector: false }
      );
    },
    trunc(value) {
      return __vulcanVectorizeUnary(value, (item) => {
        const number = __vulcanToNumber(item);
        return number == null ? null : Math.trunc(number);
      });
    },
    floor(value) {
      return __vulcanVectorizeUnary(value, (item) => {
        const number = __vulcanToNumber(item);
        return number == null ? null : Math.floor(number);
      });
    },
    ceil(value) {
      return __vulcanVectorizeUnary(value, (item) => {
        const number = __vulcanToNumber(item);
        return number == null ? null : Math.ceil(number);
      });
    },
    min(...values) {
      return __vulcanExtreme(values, false);
    },
    max(...values) {
      return __vulcanExtreme(values, true);
    },
    sum(values) {
      return __vulcanAsArray(values).reduce((total, item) => {
        const number = __vulcanToNumber(item);
        return number == null ? total : total + number;
      }, 0);
    },
    product(values) {
      return __vulcanAsArray(values).reduce((total, item) => {
        const number = __vulcanToNumber(item);
        return number == null ? total : total * number;
      }, 1);
    },
    average(values) {
      const numbers = __vulcanAsArray(values)
        .map(__vulcanToNumber)
        .filter((item) => item != null);
      if (numbers.length === 0) {
        return null;
      }
      return numbers.reduce((total, item) => total + item, 0) / numbers.length;
    },
    reduce(values, reducer) {
      const items = __vulcanAsArray(values);
      if (items.length === 0) {
        return null;
      }
      return items.slice(1).reduce((accumulator, item) => {
        if (typeof reducer === "function") {
          return reducer(accumulator, item);
        }
        return __vulcanReduceOperator(accumulator, item, String(reducer));
      }, items[0]);
    },
    minby(values, callback) {
      return __vulcanAsArray(values).reduce((best, item) => {
        if (best === null) {
          return item;
        }
        return dv.compare(callback(item), callback(best)) < 0 ? item : best;
      }, null);
    },
    maxby(values, callback) {
      return __vulcanAsArray(values).reduce((best, item) => {
        if (best === null) {
          return item;
        }
        return dv.compare(callback(item), callback(best)) > 0 ? item : best;
      }, null);
    },
    length(value) {
      if (value instanceof DataArray) {
        return value.length;
      }
      if (Array.isArray(value)) {
        return value.length;
      }
      if (typeof value === "string") {
        return value.length;
      }
      if (value && typeof value === "object") {
        return Object.keys(value).length;
      }
      return value == null ? 0 : null;
    },
    sort(value) {
      const items = [...__vulcanAsArray(value)];
      items.sort((left, right) => __vulcanCompareForSort(left, right, "asc"));
      return __vulcanWrapLike(value, items);
    },
    reverse(value) {
      return __vulcanWrapLike(value, [...__vulcanAsArray(value)].reverse());
    },
    unique(value) {
      const seen = new Set();
      const unique = [];
      for (const item of __vulcanAsArray(value)) {
        const serialized = JSON.stringify(__vulcanPlain(item));
        if (!seen.has(serialized)) {
          seen.add(serialized);
          unique.push(item);
        }
      }
      return __vulcanWrapLike(value, unique);
    },
    flat(value, depth = 1) {
      return __vulcanWrapLike(value, __vulcanAsArray(value).flat(Number(depth ?? 1)));
    },
    slice(value, start, end) {
      return __vulcanWrapLike(value, __vulcanAsArray(value).slice(start, end));
    },
    nonnull(value) {
      return __vulcanWrapLike(
        value,
        __vulcanAsArray(value).filter((item) => item != null)
      );
    },
    firstvalue(value) {
      return __vulcanAsArray(value).find((item) => item != null) ?? null;
    },
    contains(haystack, needle) {
      return __vulcanVectorizeBinary(
        haystack,
        needle,
        (left, right) => __vulcanContains(left, right),
        { rightVector: false }
      );
    },
    icontains(haystack, needle) {
      return __vulcanVectorizeBinary(
        haystack,
        needle,
        (left, right) => __vulcanContains(left, right, { insensitive: true }),
        { rightVector: false }
      );
    },
    econtains(haystack, needle) {
      return __vulcanVectorizeBinary(
        haystack,
        needle,
        (left, right) => __vulcanContains(left, right, { exact: true }),
        { rightVector: false }
      );
    },
    containsword(value, needle) {
      return __vulcanVectorizeBinary(
        value,
        needle,
        (text, word) => {
          if (typeof text !== "string" || word == null) {
            return null;
          }
          const regex = new RegExp(`\\b${__vulcanEscapeRegExp(word)}\\b`, "i");
          return regex.test(text);
        },
        { rightVector: false }
      );
    },
    all(values, predicate) {
      const items = __vulcanAsArray(values);
      return predicate
        ? items.every((item, index) => __vulcanTruthy(predicate(item, index)))
        : items.every(__vulcanTruthy);
    },
    any(values, predicate) {
      const items = __vulcanAsArray(values);
      return predicate
        ? items.some((item, index) => __vulcanTruthy(predicate(item, index)))
        : items.some(__vulcanTruthy);
    },
    none(values, predicate) {
      return !this.any(values, predicate);
    },
    filter(values, predicate) {
      return __vulcanWrapLike(
        values,
        __vulcanAsArray(values).filter((item, index) => predicate(item, index))
      );
    },
    map(values, predicate) {
      return __vulcanWrapLike(
        values,
        __vulcanAsArray(values).map((item, index) => predicate(item, index))
      );
    },
    join(values, separator = ", ") {
      return __vulcanAsArray(values).map(__vulcanRenderScalar).join(separator);
    },
    lower(value) {
      return __vulcanVectorizeUnary(value, (item) =>
        typeof item === "string" ? item.toLowerCase() : item == null ? null : null
      );
    },
    upper(value) {
      return __vulcanVectorizeUnary(value, (item) =>
        typeof item === "string" ? item.toUpperCase() : item == null ? null : null
      );
    },
    startswith(value, prefix) {
      return __vulcanVectorizeBinary(
        value,
        prefix,
        (text, needle) =>
          typeof text === "string" && typeof needle === "string"
            ? text.startsWith(needle)
            : text == null || needle == null ? null : null,
        { rightVector: false }
      );
    },
    endswith(value, suffix) {
      return __vulcanVectorizeBinary(
        value,
        suffix,
        (text, needle) =>
          typeof text === "string" && typeof needle === "string"
            ? text.endsWith(needle)
            : text == null || needle == null ? null : null,
        { rightVector: false }
      );
    },
    substring(value, start, end) {
      return __vulcanVectorizeTernary(
        value,
        start,
        end,
        (text, startIndex, endIndex) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          return text.substring(
            Number(startIndex ?? 0),
            endIndex == null ? undefined : Number(endIndex)
          );
        },
        { secondVector: false, thirdVector: false }
      );
    },
    split(value, delimiter, limit) {
      return __vulcanVectorizeTernary(
        value,
        delimiter,
        limit,
        (text, needle, maxParts) => {
          if (typeof text !== "string" || needle == null) {
            return text == null ? null : null;
          }
          const parts = text.split(String(needle));
          return maxParts == null ? parts : parts.slice(0, Number(maxParts));
        },
        { secondVector: false, thirdVector: false }
      );
    },
    replace(value, pattern, replacement) {
      return __vulcanVectorizeTernary(
        value,
        pattern,
        replacement,
        (text, needle, output) => {
          if (typeof text !== "string" || needle == null || output == null) {
            return text == null ? null : null;
          }
          return text.replaceAll(String(needle), String(output));
        },
        { secondVector: false, thirdVector: false }
      );
    },
    regextest(pattern, value) {
      return __vulcanVectorizeBinary(
        pattern,
        value,
        (regexSource, text) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          const regex = __vulcanRegex(regexSource);
          return regex == null ? null : regex.test(text);
        },
        { leftVector: false }
      );
    },
    regexmatch(pattern, value) {
      return __vulcanVectorizeBinary(
        pattern,
        value,
        (regexSource, text) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          const regex = __vulcanRegex(regexSource);
          const match = regex == null ? null : text.match(regex);
          return match == null ? null : Array.from(match);
        },
        { leftVector: false }
      );
    },
    regexreplace(value, pattern, replacement) {
      return __vulcanVectorizeTernary(
        value,
        pattern,
        replacement,
        (text, regexSource, output) => {
          if (typeof text !== "string" || output == null) {
            return text == null ? null : null;
          }
          const regex = __vulcanRegex(regexSource);
          return regex == null ? null : text.replace(regex, String(output));
        },
        { secondVector: false, thirdVector: false }
      );
    },
    truncate(value, length, suffix = "...") {
      return __vulcanVectorizeTernary(
        value,
        length,
        suffix,
        (text, maxLength, textSuffix) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          const targetLength = Number(maxLength);
          if (!Number.isFinite(targetLength) || targetLength < 0) {
            return null;
          }
          return text.length <= targetLength
            ? text
            : `${text.slice(0, targetLength)}${textSuffix == null ? "" : String(textSuffix)}`;
        },
        { secondVector: false, thirdVector: false }
      );
    },
    padleft(value, length, padding = " ") {
      return __vulcanVectorizeTernary(
        value,
        length,
        padding,
        (text, targetLength, fill) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          return text.padStart(Number(targetLength), String(fill ?? " "));
        },
        { secondVector: false, thirdVector: false }
      );
    },
    padright(value, length, padding = " ") {
      return __vulcanVectorizeTernary(
        value,
        length,
        padding,
        (text, targetLength, fill) => {
          if (typeof text !== "string") {
            return text == null ? null : null;
          }
          return text.padEnd(Number(targetLength), String(fill ?? " "));
        },
        { secondVector: false, thirdVector: false }
      );
    },
    extract(value, ...keys) {
      const extractKeys = (item) => {
        if (!item || typeof item !== "object" || Array.isArray(item)) {
          return null;
        }
        const output = {};
        for (const key of keys) {
          const normalized = String(key);
          if (Object.prototype.hasOwnProperty.call(item, normalized)) {
            output[normalized] = item[normalized];
          }
        }
        return output;
      };
      return __vulcanVectorizeUnary(value, extractKeys);
    },
    dateformat(value, format) {
      return __vulcanVectorizeBinary(
        value,
        format,
        (item, textFormat) => {
          const millis = __vulcanDateMillis(item);
          return millis == null || textFormat == null
            ? null
            : __vulcan_format_date(millis, String(textFormat));
        },
        { rightVector: false }
      );
    },
    durationformat(value, format) {
      return __vulcanVectorizeBinary(
        value,
        format,
        (item, textFormat) => {
          if (item instanceof VulcanDuration && textFormat != null) {
            return item.toFormat(String(textFormat));
          }
          const millis = __vulcanDurationMillis(item);
          return millis == null || textFormat == null
            ? null
            : __vulcan_format_duration(millis, String(textFormat));
        },
        { rightVector: false }
      );
    },
    striptime(value) {
      return __vulcanVectorizeUnary(value, (item) => {
        const millis = __vulcanDateMillis(item);
        return millis == null ? null : new Date(new Date(millis).setUTCHours(0, 0, 0, 0));
      });
    },
    localtime(value) {
      return value;
    },
    default(value, fallback) {
      return __vulcanVectorizeBinary(
        value,
        fallback,
        (item, replacement) => item == null ? replacement : item,
        { rightVector: false }
      );
    },
    ldefault(value, fallback) {
      return value == null ? fallback : value;
    },
    choice(condition, left, right) {
      return __vulcanVectorizeTernary(
        condition,
        left,
        right,
        (predicate, leftValue, rightValue) =>
          __vulcanTruthy(predicate) ? leftValue : rightValue,
        { secondVector: false, thirdVector: false }
      );
    },
    display(value) {
      return __vulcanVectorizeUnary(value, (item) =>
        item == null ? null : __vulcanRenderScalar(item)
      );
    },
    hash(seed, text = "", variant = 0) {
      const input = `${seed}|${text}|${variant}`;
      let hash = 2166136261;
      for (const character of input) {
        hash ^= character.charCodeAt(0);
        hash = Math.imul(hash, 16777619);
      }
      return `00000000${(hash >>> 0).toString(16)}`.slice(-8);
    },
    currencyformat(value, currency = "USD") {
      return __vulcanVectorizeBinary(
        value,
        currency,
        (item, code) => {
          const number = __vulcanToNumber(item);
          if (number == null || code == null) {
            return null;
          }
          try {
            return new Intl.NumberFormat(undefined, {
              style: "currency",
              currency: String(code),
            }).format(number);
          } catch {
            return null;
          }
        },
        { rightVector: false }
      );
    },
    meta(value) {
      return __vulcanVectorizeUnary(value, (item) =>
        item == null ? null : __vulcanMeta(item)
      );
    },
  };
}

const __vulcanPrivateEval = globalThis.eval;

const dv = {
  pages(source) {
    return new DataArray(__vulcanLazyPages(source).map(__vulcanRevivePage));
  },
  page(path) {
    const target = path instanceof Link ? path.path : path;
    return __vulcanRevivePage(JSON.parse(__vulcan_page_json(target)));
  },
  current() {
    return __vulcanRevivePage(JSON.parse(__vulcan_current_json()));
  },
  pagePaths(source) {
    return new DataArray(__vulcanLazyPages(source).map((page) => page.file.path));
  },
  // Dataview's query functions are asynchronous; `vault.query()` is the
  // synchronous equivalent.
  async query(dql, file, _settings) {
    const result = __vulcanQuerySync(dql, file);
    return result.successful
      ? { successful: true, value: __vulcanDataviewQueryResult(result.value, file ?? dv.currentFilePath) }
      : result;
  },
  async tryQuery(dql, file, _settings) {
    return __vulcanDataviewQueryResult(__vulcanTryQuerySync(dql, file), file ?? dv.currentFilePath);
  },
  async queryMarkdown(dql, file, settings) {
    const result = await dv.query(dql, file, settings);
    return result.successful
      ? { successful: true, value: __vulcanQueryResultMarkdown(result.value) }
      : result;
  },
  async tryQueryMarkdown(dql, file, settings) {
    return __vulcanQueryResultMarkdown(await dv.tryQuery(dql, file, settings));
  },
  async execute(dql) {
    const result = __vulcanTryQuerySync(dql);
    __vulcan_emit(JSON.stringify({ kind: "query", result }));
  },
  async executeJs(code) {
    return __vulcanPrivateEval(`(async () => {${code}\n})()`);
  },
  // Results come back typed as page fields are: dates, durations, and
  // links resolved from the current note.
  evaluate(expression, context) {
    const result = JSON.parse(
      __vulcan_evaluate_json(
        String(expression),
        context == null ? null : __vulcanSerialize(context)
      )
    );
    if (result.successful) {
      result.value = __vulcanReviveFieldValue(result.value, dv.currentFilePath);
    }
    return result;
  },
  tryEvaluate(expression, context) {
    const result = dv.evaluate(expression, context);
    if (!result.successful) {
      throw new Error(result.error);
    }
    return result.value;
  },
  table(headers, rows) {
    const normalizedRows = __vulcanAsArray(rows).map((row) => __vulcanAsArray(row).map(__vulcanPlain));
    __vulcan_emit(JSON.stringify({ kind: "table", headers: __vulcanAsArray(headers).map(String), rows: normalizedRows }));
  },
  list(items) {
    __vulcan_emit(JSON.stringify({ kind: "list", items: __vulcanAsArray(items).map(__vulcanPlain) }));
  },
  // As in Dataview, tasks are grouped by file unless `groupByFile` is
  // false, and a task whose parent is also listed shows only beneath it.
  taskList(tasks, groupByFile = true) {
    const items = __vulcanAsArray(tasks);
    const lineKey = (item, line) => `${item?.path ?? ""}:${line}`;
    const present = new Set(items.map((item) => lineKey(item, item?.line)));
    const roots = items.filter(
      (item) => !(item && item.parent != null && present.has(lineKey(item, item.parent)))
    );
    __vulcan_emit(JSON.stringify({ kind: "task_list", tasks: roots.map(__vulcanPlain), group_by_file: !!groupByFile }));
  },
  paragraph(text) {
    __vulcan_emit(JSON.stringify({ kind: "paragraph", text: String(text) }));
  },
  header(level, text) {
    __vulcan_emit(JSON.stringify({ kind: "header", level: Number(level), text: String(text) }));
  },
  el(element, text, attrs = {}) {
    __vulcan_emit(JSON.stringify({ kind: "element", element: String(element), text: String(text), attrs: __vulcanPlain(attrs) }));
  },
  span(text) {
    __vulcan_emit(JSON.stringify({ kind: "span", text: String(text) }));
  },
  container: {
    classes: [],
    addClass(name) {
      if (!this.classes.includes(name)) {
        this.classes.push(name);
      }
    },
    removeClass(name) {
      this.classes = this.classes.filter((candidate) => candidate !== name);
    },
  },
  io: {
    async load(path, _originFile) {
      return __vulcan_load(path);
    },
    async csv(path, originFile) {
      return new DataArray(JSON.parse(__vulcan_csv_json(path, originFile)));
    },
    normalize(path, originFile) {
      return __vulcan_normalize(path, originFile);
    },
  },
  // Dataview runs a view as an async function when it uses `await`.
  async view(path, input) {
    const source = __vulcan_load_view(path);
    const previousInput = globalThis.input;
    try {
      globalThis.input = input ?? null;
      return await __vulcanPrivateEval(
        source.includes("await") ? `(async () => {${source}\n})()` : source
      );
    } finally {
      globalThis.input = previousInput;
    }
  },
  array(raw) {
    return raw instanceof DataArray ? raw : new DataArray(__vulcanAsArray(raw));
  },
  isArray(raw) {
    return raw instanceof DataArray || Array.isArray(raw);
  },
  isDataArray(raw) {
    return raw instanceof DataArray;
  },
  // Dataview's date(): dates pass through, and strings may be the
  // shorthands `today`, `now`, `sow`, `end-of-month`, and so on. A link
  // gives the date in its display or path, or its note's `file.day`.
  date(input) {
    if (input == null) {
      return null;
    }
    if (input instanceof VulcanDateTime) {
      return input;
    }
    if (input instanceof Link) {
      return (
        __vulcanParseDataviewDate(input.display) ??
        __vulcanParseDataviewDate(input.path) ??
        dv.page(input)?.file?.day ??
        null
      );
    }
    if (typeof input === "string") {
      const shorthand = __vulcan_date_shorthand_millis(input.trim());
      if (shorthand != null) {
        return new VulcanDateTime(new Date(shorthand));
      }
      const parsed = __vulcanParseDataviewDate(input.trim());
      if (parsed !== null) {
        return parsed;
      }
    }
    const millis = __vulcan_date_millis(
      input instanceof Date ? input.toISOString() : String(input)
    );
    return millis == null ? null : new VulcanDateTime(new Date(millis));
  },
  // Dataview's dur(): `3 days`, `1h 30m`, `2 hours, 5 minutes`; an ISO
  // duration or Vulcan's duration text also works.
  duration(input) {
    return __vulcanVectorizeUnary(input, (item) => {
      if (item == null) {
        return null;
      }
      if (item instanceof VulcanDuration) {
        return item;
      }
      const text = String(item).trim();
      const parsed = __vulcanParseDuration(text) ?? VulcanDuration.fromISO(text);
      if (parsed !== null) {
        return parsed;
      }
      const millis = __vulcan_duration_millis(text);
      return millis == null ? null : VulcanDuration.fromMillis(millis);
    });
  },
  // Dataview's parse(): the inline-field grammar, so `2026-01-02`,
  // `3 days`, `[[Note]]`, `#tag`, `true`, `"text"`, and comma lists.
  parse(value) {
    return __vulcanParseInlineValue(value);
  },
  // Dataview's literal(): frontmatter typing for plain JS values.
  literal(value) {
    return __vulcanParseFrontmatter(value);
  },
  compare(left, right) {
    return __vulcan_compare(
      __vulcanSerializeComparable(left),
      __vulcanSerializeComparable(right)
    );
  },
  equal(left, right) {
    return __vulcan_equal(
      __vulcanSerializeComparable(left),
      __vulcanSerializeComparable(right)
    );
  },
  clone(value) {
    return __vulcanClone(value);
  },
  fileLink(path, embed = false, display = undefined) {
    return Link.file(path, embed, display);
  },
  sectionLink(path, section, embed = false, display = undefined) {
    return Link.header(path, section, embed, display);
  },
  blockLink(path, blockId, embed = false, display = undefined) {
    return Link.block(path, blockId, embed, display);
  },
  markdownTable(headers, values, _settings) {
    return __vulcanMarkdownTable(
      __vulcanAsArray(headers ?? []),
      __vulcanAsArray(values ?? []).map(__vulcanAsArray)
    );
  },
  markdownList(values, _settings) {
    return __vulcanMarkdownList(__vulcanAsArray(values ?? []), 0);
  },
  markdownTaskList(tasks, _settings) {
    return __vulcanMarkdownTaskList(__vulcanAsArray(tasks ?? []), 0);
  },
  currentFilePath: globalThis.__vulcan_current_file ?? "",
  func: null,
  luxon: {
    DateTime: VulcanDateTime,
    Duration: VulcanDuration,
  },
};

dv.func = buildDvFunctions(dv);
const __vulcanJsHelp = new Map();

function __vulcanRegisterHelp(target, text) {
  if (target != null && text) {
    __vulcanJsHelp.set(target, String(text).trim());
  }
}

function __vulcanHydrateAny(value) {
  if (Array.isArray(value)) {
    return value.map(__vulcanHydrateAny);
  }
  if (value && typeof value === "object" && value.file?.path) {
    return new Note(value);
  }
  return value;
}

function __vulcanHydrateRelationship(value) {
  if (!value || typeof value !== "object") {
    return value;
  }
  const hydrated = { ...value };
  if (hydrated.note?.file?.path) {
    hydrated.note = new Note(hydrated.note);
  }
  return hydrated;
}

class Note {
  constructor(data = {}) {
    this.__data = data ?? {};
    this.__details = null;
    return new Proxy(this, {
      get(target, prop, receiver) {
        if (prop in target) {
          const value = Reflect.get(target, prop, receiver);
          return typeof value === "function" ? value.bind(target) : value;
        }
        if (typeof prop === "string" && prop in target.__data) {
          return __vulcanHydrateAny(target.__data[prop]);
        }
        return undefined;
      },
      has(target, prop) {
        return prop in target || prop in target.__data;
      },
      ownKeys(target) {
        return Reflect.ownKeys(target.__data);
      },
      getOwnPropertyDescriptor(target, prop) {
        if (typeof prop === "string" && prop in target.__data) {
          return { configurable: true, enumerable: true };
        }
        return Object.getOwnPropertyDescriptor(target, prop);
      }
    });
  }

  __ensureDetails() {
    if (this.__details == null) {
      this.__details = JSON.parse(__vulcan_note_details_json(this.file.path));
      if (this.__details?.page && typeof this.__details.page === "object") {
        this.__data = { ...this.__data, ...this.__details.page };
      }
      if (this.__details?.file && typeof this.__details.file === "object") {
        this.__data.file = this.__details.file;
      }
    }
    return this.__details ?? {};
  }

  get file() {
    return this.__data.file ?? {};
  }

  get path() {
    return this.file.path ?? null;
  }

  get name() {
    return this.file.name ?? null;
  }

  get content() {
    return this.__ensureDetails().content ?? "";
  }

  get html() {
    return this.__ensureDetails().html ?? "";
  }

  get frontmatter() {
    return this.file.frontmatter ?? this.__ensureDetails().frontmatter ?? {};
  }

  get tags() {
    return this.file.tags ?? [];
  }

  get aliases() {
    return this.file.aliases ?? [];
  }

  get headings() {
    return this.__ensureDetails().headings ?? [];
  }

  get blocks() {
    return this.__ensureDetails().blocks ?? [];
  }

  outline() {
    return this.__ensureDetails().outline ?? { total_lines: 0, sections: [], block_refs: [] };
  }

  get tasks() {
    return this.file.tasks ?? [];
  }

  get dataview_fields() {
    return this.__ensureDetails().dataview_fields ?? [];
  }

  read(opts = {}) {
    return JSON.parse(
      __vulcan_note_read_json(this.file.path, JSON.stringify(__vulcanPlain(opts)))
    );
  }

  section(opts = {}) {
    return this.read(opts);
  }

  block(id) {
    return this.read({ block_ref: String(id) });
  }

  patch(find, replace, opts = {}) {
    return vault.patch(this.file.path, find, replace, opts);
  }

  append(text, opts = {}) {
    return vault.append(this.file.path, text, opts);
  }

  update(key, value) {
    return vault.update(this.file.path, key, value);
  }

  unset(key) {
    return vault.unset(this.file.path, key);
  }

  get properties() {
    const note = this;
    return {
      get(key) {
        return note.frontmatter?.[key] ?? note.__data?.[key] ?? null;
      },
      set(key, value) {
        return vault.update(note.file.path, key, value);
      },
      unset(key) {
        return vault.unset(note.file.path, key);
      },
      merge(values = {}) {
        let updated = note;
        for (const [key, value] of Object.entries(values ?? {})) {
          updated = vault.update(note.file.path, key, value);
        }
        return updated;
      },
    };
  }

  links() {
    return new DataArray(
      JSON.parse(__vulcan_note_links_json(this.file.path, "outgoing")).map(__vulcanHydrateRelationship)
    );
  }

  backlinks() {
    return new DataArray(
      JSON.parse(__vulcan_note_links_json(this.file.path, "incoming")).map(__vulcanHydrateRelationship)
    );
  }

  neighbors(depth = 1) {
    return new DataArray(
      JSON.parse(__vulcan_note_neighbors_json(this.file.path, Number(depth) || 1)).map(
        __vulcanHydrateAny
      )
    );
  }

  toJSON() {
    const merged = { ...this.__data };
    if (this.__details != null) {
      merged.content = this.__details.content ?? merged.content;
      merged.html = this.__details.html ?? merged.html;
      merged.frontmatter = this.__details.frontmatter ?? merged.frontmatter;
      merged.headings = this.__details.headings ?? merged.headings;
      merged.blocks = this.__details.blocks ?? merged.blocks;
      merged.outline = this.__details.outline ?? merged.outline;
      merged.dataview_fields = this.__details.dataview_fields ?? merged.dataview_fields;
    }
    return merged;
  }
}

function __vulcanNote(value) {
  return value == null ? null : new Note(value);
}

function __vulcanNotes(values) {
  return new DataArray(__vulcanAsArray(values).map(__vulcanNote));
}

function __vulcanMutation(kind, payload) {
  return JSON.parse(__vulcan_mutate_json(String(kind), JSON.stringify(__vulcanPlain(payload))));
}

function __vulcanGetPath(value) {
  if (value instanceof Note) {
    return value.file.path;
  }
  if (value && typeof value === "object" && value.file?.path) {
    return value.file.path;
  }
  return String(value);
}

function __vulcanUnifiedDiff(before, after, path = "content") {
  const beforeLines = String(before ?? "").split("\n");
  const afterLines = String(after ?? "").split("\n");
  if (beforeLines.length && beforeLines[beforeLines.length - 1] === "") beforeLines.pop();
  if (afterLines.length && afterLines[afterLines.length - 1] === "") afterLines.pop();
  const lines = [`--- ${path}`, `+++ ${path}`];
  const max = Math.max(beforeLines.length, afterLines.length);
  for (let index = 0; index < max; index += 1) {
    const left = beforeLines[index];
    const right = afterLines[index];
    if (left === right) {
      lines.push(` ${left ?? ""}`);
    } else {
      if (left !== undefined) lines.push(`-${left}`);
      if (right !== undefined) lines.push(`+${right}`);
    }
  }
  return lines.join("\n");
}

class VaultMutationPlan {
  constructor(opts = {}) {
    this.dryRun = !!(opts?.dry_run ?? opts?.dryRun);
    this.operations = [];
  }

  set(path, content, opts = {}) {
    this.operations.push({ kind: "set", path: __vulcanGetPath(path), content, opts });
    return this;
  }

  writeNote(path, content, opts = {}) {
    return this.set(path, content, opts);
  }

  create(path, opts = {}) {
    this.operations.push({ kind: "create", path: __vulcanGetPath(path), opts });
    return this;
  }

  append(path, text, opts = {}) {
    this.operations.push({ kind: "append", path: __vulcanGetPath(path), text, opts });
    return this;
  }

  appendNote(path, text, opts = {}) {
    return this.append(path, text, opts);
  }

  patch(path, find, replace, opts = {}) {
    this.operations.push({ kind: "patch", path: __vulcanGetPath(path), find, replace, opts });
    return this;
  }

  update(path, key, value) {
    this.operations.push({ kind: "update", path: __vulcanGetPath(path), key, value });
    return this;
  }

  unset(path, key) {
    this.operations.push({ kind: "unset", path: __vulcanGetPath(path), key });
    return this;
  }

  diff(path, before, after) {
    this.operations.push({ kind: "diff", path: __vulcanGetPath(path), before, after });
    return this;
  }

  preview() {
    const changedPaths = [...new Set(this.operations.map((op) => op.path).filter(Boolean))];
    return {
      ok: true,
      dry_run: true,
      applied: false,
      changed_paths: changedPaths,
      operations: this.operations.map(__vulcanPlain),
      diffs: this.operations
        .filter((op) => op.kind === "diff")
        .map((op) => ({ path: op.path, diff: __vulcanUnifiedDiff(op.before, op.after, op.path) })),
    };
  }

  apply() {
    if (this.dryRun) {
      return this.preview();
    }
    const results = [];
    vault.transaction(() => {
      for (const op of this.operations) {
        if (op.kind === "set") results.push(vault.set(op.path, op.content, op.opts));
        if (op.kind === "create") results.push(vault.create(op.path, op.opts));
        if (op.kind === "append") results.push(vault.append(op.path, op.text, op.opts));
        if (op.kind === "patch") results.push(vault.patch(op.path, op.find, op.replace, op.opts));
        if (op.kind === "update") results.push(vault.update(op.path, op.key, op.value));
        if (op.kind === "unset") results.push(vault.unset(op.path, op.key));
      }
    });
    const changedPaths = [...new Set(this.operations.map((op) => op.path).filter(Boolean))];
    return {
      ok: true,
      dry_run: false,
      applied: true,
      changed_paths: changedPaths,
      operations: this.operations.map(__vulcanPlain),
      results,
    };
  }

  result() {
    return this.apply();
  }
}

class VaultQueryBuilder {
  constructor() {
    this.source = "";
    this.filters = [];
    this.sorts = [];
    this.limitValue = null;
  }

  from(source) {
    this.source = String(source);
    return this;
  }

  tag(tag) {
    this.source = "#" + String(tag).replace(/^#/, "");
    return this;
  }

  path(path) {
    this.source = `"${String(path).replace(/"/g, '\\"')}"`;
    return this;
  }

  where(field, op, value) {
    const rendered = typeof value === "string" ? `"${value.replace(/"/g, '\\"')}"` : String(value);
    this.filters.push(`${field} ${op} ${rendered}`);
    return this;
  }

  sort(field, direction = "ASC") {
    this.sorts.push(`${field} ${String(direction).toUpperCase()}`);
    return this;
  }

  limit(value) {
    this.limitValue = Number(value);
    return this;
  }

  toString() {
    const parts = ["TABLE file.link"];
    if (this.source) parts.push("FROM", this.source);
    for (const filter of this.filters) parts.push("WHERE", filter);
    if (this.sorts.length) parts.push("SORT", this.sorts.join(", "));
    if (this.limitValue != null) parts.push("LIMIT", String(this.limitValue));
    return parts.join(" ");
  }

  run(opts = {}) {
    return vault.query(this.toString(), opts);
  }
}

class VaultSearchBuilder {
  constructor() {
    this.parts = [];
    this.limitValue = null;
  }

  text(value) {
    this.parts.push(String(value));
    return this;
  }

  tag(value) {
    this.parts.push(`tag:${String(value).replace(/^#/, "")}`);
    return this;
  }

  path(value) {
    this.parts.push(`path:${String(value)}`);
    return this;
  }

  limit(value) {
    this.limitValue = Number(value);
    return this;
  }

  toString() {
    return this.parts.join(" ").trim();
  }

  run(opts = {}) {
    return vault.search(this.toString(), { ...opts, limit: opts?.limit ?? this.limitValue });
  }
}

const vault = {
  note(path) {
    return __vulcanNote(JSON.parse(__vulcan_page_json(path)));
  },
  notes(source) {
    return __vulcanNotes(JSON.parse(__vulcan_pages_json(source)));
  },
  query(dql, opts = {}) {
    if (dql == null) {
      return new VaultQueryBuilder();
    }
    return __vulcanTryQuerySync(dql, opts?.file ?? null);
  },
  search(query, opts = {}) {
    if (query == null) {
      return new VaultSearchBuilder();
    }
    return JSON.parse(__vulcan_search_json(String(query), opts?.limit ?? null));
  },
  diffText(before, after, opts = {}) {
    return __vulcanUnifiedDiff(before, after, opts?.path ?? "content");
  },
  mutation(opts = {}) {
    return new VaultMutationPlan(opts);
  },
  plan(opts = {}) {
    return new VaultMutationPlan(opts);
  },
  withLock(_paths, callback) {
    return callback();
  },
  set(path, content, opts = {}) {
    return __vulcanNote(
      __vulcanMutation("set", {
        path,
        content,
        preserveFrontmatter: !!(opts?.preserveFrontmatter ?? opts?.noFrontmatter),
      }).note
    );
  },
  create(path, opts = {}) {
    // A string is the content, as in `vault.set(path, content)`.
    const options = typeof opts === "string" ? { content: opts } : opts;
    return __vulcanNote(
      __vulcanMutation("create", {
        path,
        content: options?.content ?? "",
        frontmatter: options?.frontmatter ?? null,
      }).note
    );
  },
  append(path, text, opts = {}) {
    return __vulcanNote(
      __vulcanMutation("append", {
        path,
        text,
        heading: opts?.heading ?? null,
        prepend: !!opts?.prepend,
      }).note
    );
  },
  patch(path, find, replace, opts = {}) {
    const regex = find instanceof RegExp;
    const response = __vulcanMutation("patch", {
      path,
      find: regex ? find.source : String(find),
      replace,
      regex,
      replaceAll: !!opts?.all,
    });
    return response.note ? __vulcanNote(response.note) : response;
  },
  update(path, key, value) {
    return __vulcanNote(
      __vulcanMutation("update", { path, key, value }).note
    );
  },
  unset(path, key) {
    return __vulcanNote(
      __vulcanMutation("unset", { path, key }).note
    );
  },
  inbox(text) {
    return __vulcanMutation("inbox", { text });
  },
  transaction(callback) {
    __vulcan_tx_begin();
    const tx = {
      set: (...args) => vault.set(...args),
      create: (...args) => vault.create(...args),
      append: (...args) => vault.append(...args),
      patch: (...args) => vault.patch(...args),
      update: (...args) => vault.update(...args),
      unset: (...args) => vault.unset(...args),
      inbox: (...args) => vault.inbox(...args),
      daily: {
        append: (...args) => vault.daily.append(...args),
      },
      refactor: null,
    };
    tx.refactor = vault.refactor;
    try {
      const result = callback(tx);
      __vulcan_tx_commit();
      return result;
    } catch (error) {
      __vulcan_tx_rollback();
      throw error;
    }
  },
  refactor: {
    renameAlias(note, oldAlias, newAlias) {
      return __vulcanMutation("refactor.renameAlias", {
        note,
        oldAlias,
        newAlias,
      });
    },
    renameHeading(note, oldHeading, newHeading) {
      return __vulcanMutation("refactor.renameHeading", {
        note,
        oldHeading,
        newHeading,
      });
    },
    renameBlockRef(note, oldBlockId, newBlockId) {
      return __vulcanMutation("refactor.renameBlockRef", {
        note,
        oldBlockId,
        newBlockId,
      });
    },
    renameProperty(oldKey, newKey) {
      return __vulcanMutation("refactor.renameProperty", { oldKey, newKey });
    },
    mergeTags(fromTag, toTag) {
      return __vulcanMutation("refactor.mergeTags", { fromTag, toTag });
    },
    move(source, destination) {
      return __vulcanMutation("refactor.move", { source, destination });
    },
  },
  graph: {
    shortestPath(from, to) {
      return JSON.parse(__vulcan_graph_path_json(String(from), String(to)));
    },
    hubs(opts = {}) {
      return JSON.parse(__vulcan_graph_hubs_json(opts?.limit ?? null));
    },
    components(opts = {}) {
      return JSON.parse(__vulcan_graph_components_json(opts?.limit ?? null));
    },
    deadEnds(opts = {}) {
      return JSON.parse(__vulcan_graph_dead_ends_json(opts?.limit ?? null));
    },
  },
  daily: {
    today() {
      return __vulcanNote(JSON.parse(__vulcan_vault_daily_json(__vulcan_today())));
    },
    path(date = "today") {
      return __vulcan_daily_path(String(date));
    },
    get(date) {
      return __vulcanNote(JSON.parse(__vulcan_vault_daily_json(String(date))));
    },
    range(from, to) {
      return __vulcanNotes(JSON.parse(__vulcan_vault_daily_range_json(String(from), String(to))));
    },
    append(text, opts = {}) {
      return __vulcanNote(
        __vulcanMutation("daily.append", {
          text,
          heading: opts?.heading ?? null,
          date: opts?.date ?? null,
        }).note
      );
    },
  },
  events(options = {}) {
    return new DataArray(
      JSON.parse(__vulcan_vault_events_json(options?.from ?? null, options?.to ?? null))
    );
  },
};

function __vulcanPermissionCheck(action, path = null) {
  return JSON.parse(__vulcan_permission_check_json(String(action), path == null ? null : String(path)));
}

const vulcan = {
  permissions() {
    const summary = JSON.parse(__vulcan_permissions_json());
    summary.canRead = (path) => __vulcanPermissionCheck("read", path).allowed;
    summary.canWrite = (path) => __vulcanPermissionCheck("write", path).allowed;
    summary.canRefactor = (path) => __vulcanPermissionCheck("refactor", path).allowed;
    summary.can = (capability, path = null) => __vulcanPermissionCheck(capability, path).allowed;
    return summary;
  },
  date: {
    today() {
      return __vulcan_today();
    },
    parse(input) {
      const millis = __vulcan_date_millis(String(input));
      return millis == null ? null : new VulcanDateTime(new Date(millis));
    },
    format(input, format = "yyyy-MM-dd") {
      const millis = input instanceof VulcanDateTime ? input.toMillis() : __vulcan_date_millis(String(input));
      return millis == null ? null : __vulcan_format_date(millis, String(format));
    },
  },
  tempDir() {
    const files = new Map();
    return {
      writeJson(path, value) {
        files.set(String(path), JSON.stringify(__vulcanPlain(value), null, 2));
        return String(path);
      },
      writeText(path, value) {
        files.set(String(path), String(value));
        return String(path);
      },
      readText(path) {
        return files.get(String(path)) ?? null;
      },
      readJson(path) {
        const value = files.get(String(path));
        return value == null ? null : JSON.parse(value);
      },
      list() {
        return Array.from(files.keys());
      },
    };
  },
};

class ToolResultBuilder {
  constructor(ok = true) {
    this.payload = { ok };
  }

  summary(text) {
    this.payload.summary = String(text);
    return this;
  }

  changedPath(path) {
    if (!this.payload.changed_paths) this.payload.changed_paths = [];
    this.payload.changed_paths.push(String(path));
    return this;
  }

  diff(diff) {
    if (!this.payload.diffs) this.payload.diffs = [];
    this.payload.diffs.push(diff);
    return this;
  }

  data(value) {
    this.payload.data = value;
    return this;
  }

  error(code, message, details = undefined) {
    this.payload.ok = false;
    this.payload.error = { code: String(code), message: String(message) };
    if (details !== undefined) this.payload.error.details = details;
    return this;
  }

  ok(value = undefined) {
    if (value && typeof value === "object") {
      Object.assign(this.payload, value);
    }
    return this.payload;
  }
}

const tool = {
  input(defaults = {}) {
    return { ...defaults, ...(globalThis.input ?? {}) };
  },
  boolean(name, fallback = false) {
    const value = (globalThis.input ?? {})[name];
    return value == null ? !!fallback : !!value;
  },
  result() {
    return new ToolResultBuilder(true);
  },
  ok(value = {}) {
    return new ToolResultBuilder(true).ok(value);
  },
  error(code, message, details = undefined) {
    return new ToolResultBuilder(false).error(code, message, details).ok();
  },
  progress(message, data = {}) {
    __vulcan_emit(JSON.stringify({ kind: "paragraph", text: String(message) }));
    return { kind: "progress", message: String(message), data };
  },
  notice(message, data = {}) {
    __vulcan_emit(JSON.stringify({ kind: "paragraph", text: String(message) }));
    return { kind: "notice", message: String(message), data };
  },
  config(defaults = {}) {
    return { ...defaults };
  },
  audit(event = {}) {
    return { kind: "audit", ...__vulcanPlain(event) };
  },
  output(value) {
    return value;
  },
  confirm(request = {}) {
    throw new Error("tool.confirm() requires an interactive approval-capable host");
  },
  idempotencyKey(value = globalThis.input ?? {}) {
    return JSON.stringify(__vulcanPlain(value));
  },
};

const web = {
  search(query, opts = {}) {
    return JSON.parse(
      __vulcan_web_search_json(String(query), opts?.limit ?? null)
    );
  },
  fetch(url, opts = {}) {
    return JSON.parse(__vulcan_web_fetch_json(String(url), opts?.mode ?? "markdown"));
  },
};

// Scripts and the REPL print console output. A rendered note does not
// show it, as Obsidian sends it to the developer console.
function __vulcanConsoleWrite(...args) {
  if (globalThis.__vulcan_rendering) {
    return;
  }
  __vulcan_emit(JSON.stringify({
    kind: "paragraph",
    text: args.map((value) => __vulcanRenderScalar(value)).join(" ")
  }));
}

const console = {
  log: __vulcanConsoleWrite,
  info: __vulcanConsoleWrite,
  warn: __vulcanConsoleWrite,
  error: __vulcanConsoleWrite,
  debug: __vulcanConsoleWrite,
};

const tools = {
  list() {
    return JSON.parse(__vulcan_tools_list_json());
  },
  get(name) {
    return JSON.parse(__vulcan_tools_get_json(String(name)));
  },
  call(name, input = {}, opts = undefined) {
    return JSON.parse(
      __vulcan_tools_call_json(
        String(name),
        JSON.stringify(input),
        JSON.stringify(opts ?? null)
      )
    );
  },
  callChecked(name, input = {}, opts = undefined) {
    const result = tools.call(name, input, opts);
    if (result && typeof result === "object") {
      Object.defineProperty(result, "expect", {
        enumerable: false,
        value(path) {
          const value = String(path)
            .split(".")
            .filter(Boolean)
            .reduce((current, key) => current?.[key], result);
          if (value === undefined || value === null) {
            throw new Error(`tools.callChecked(${name}) missing expected path: ${path}`);
          }
          return value;
        },
      });
    }
    return result;
  },
};

function __vulcanSkillToolName(skillName, commandId) {
  const normalize = (value) => String(value)
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toLowerCase();
  return "skill_" + normalize(skillName) + "_" + normalize(commandId);
}

const skills = {
  list() {
    return tools.list().filter((tool) => String(tool.name ?? "").startsWith("skill_"));
  },
  get(name) {
    return tools.get(String(name));
  },
  commands(skillName = undefined) {
    const prefix = skillName == null ? "skill_" : "skill_" + __vulcanSkillToolName(skillName, "").slice(6);
    return skills.list().filter((tool) => String(tool.name ?? "").startsWith(prefix));
  },
  run(skillName, commandId, input = {}, opts = undefined) {
    return tools.call(__vulcanSkillToolName(skillName, commandId), input, opts);
  },
};

const host = {
  exec(argv, opts = {}) {
    return JSON.parse(
      __vulcan_host_exec_json(JSON.stringify(argv), JSON.stringify(opts ?? null))
    );
  },
  shell(command, opts = {}) {
    return JSON.parse(
      __vulcan_host_shell_json(String(command), JSON.stringify(opts ?? null))
    );
  },
};

function help(obj) {
  if (obj === undefined) {
    return [
      "Vulcan JS Runtime — available globals:",
      "  vault    — note reading, writing, search, graph, daily notes",
      "  vulcan   — permission, date, and scratch helpers",
      "  dv       — Dataview-compatible query API (dv.pages, dv.table, etc.)",
      "  tools    — registry-backed skill command tools (tools.list, tools.get, tools.call)",
      "  tool     — custom-tool input, result, progress, and audit helpers",
      "  host     — permission-gated host process execution (host.exec, host.shell)",
      "  web      — external web search and fetch (requires --sandbox net)",
      "  console  — console.log() for output",
      "  app      — Obsidian API compatibility stub (app.vault.*)",
      "  _        — last successful result",
      "  _error   — last error message",
      "",
      "Usage: help(vault), help(vulcan.permissions), help(tool.result), etc.",
    ].join("\n");
  }
  if (typeof obj === "function" && !__vulcanJsHelp.has(obj)) {
    const name = obj.name || "this function";
    return 'Tip: "' + name + '" is a function — did you mean ' + name + '() ?';
  }
  return __vulcanJsHelp.get(obj) ?? "No help available for this object.";
}

__vulcanRegisterHelp(
  vault.note,
  `vault.note(path: string): Note

Resolve one note and return a rich Note object.

Parameters:
  path - Vault-relative path, filename, or alias-like note identifier.

Example:
  vault.note("Projects/Alpha").content  // raw markdown from disk

Note properties:
  note.content       - raw markdown source
  note.html          - rendered HTML using Vulcan's markdown pipeline
  note.outline()     - section and block outline with semantic ids and line spans
  note.read(opts)    - partial note read using heading/section/block/line/match selectors
  note.section(opts) - alias for note.read(opts)
  note.properties.*  - frontmatter get/set/unset/merge helpers

See also: vault.notes(), vault.query()`
);
__vulcanRegisterHelp(
  vault.notes,
  `vault.notes(source?: string): DataArray<Note>

Return note collections compatible with DataArray chaining.

Parameters:
  source - Optional Dataview-style source selector such as a folder or tag filter.

Example:
  vault.notes('"Projects"').where((note) => note.status === "active").limit(5)

See also: vault.note(), vault.query()`
);
__vulcanRegisterHelp(
  vault.query,
  `vault.query(dql?: string, opts?: { file?: string }): QueryResult | QueryBuilder

Run one DQL query against the indexed vault, or call without arguments to build one.

Parameters:
  dql  - Query DSL string.
  opts - Optional execution settings such as the current file context.

Example:
  vault.query('TABLE status FROM "Projects"')
  vault.query().path("Projects").where("status", "=", "active").limit(5).run()

See also: vault.notes(), vault.search()`
);
__vulcanRegisterHelp(
  vault.search,
  `vault.search(query?: string, opts?: { limit?: number }): SearchReport | SearchBuilder

Run indexed full-text search, or call without arguments to build one.

Parameters:
  query - Search text or phrase.
  opts  - Optional result controls such as limit.

Example:
  vault.search("Alpha", { limit: 3 })
  vault.search().text("Alpha").path("Projects").limit(3).run()

See also: vault.notes(), vault.query()`
);
__vulcanRegisterHelp(
  vault.plan,
  `vault.plan(opts?: { dry_run?: boolean }): MutationPlan

Build a reusable mutation plan for custom tools. Dry-run plans return changed_paths,
operations, and explicit diffs without writing files.

Example:
  vault.plan({ dry_run: input.dry_run }).append("Daily.md", "- [ ] Review").result()

See also: vault.mutation(), tool.result(), vulcan.permissions()`
);
__vulcanRegisterHelp(vault.mutation, __vulcanJsHelp.get(vault.plan));
__vulcanRegisterHelp(
  vault.diffText,
  `vault.diffText(before: string, after: string, opts?: { path?: string }): string

Return a small unified diff string suitable for dry-run output.

Example:
  vault.diffText(oldText, newText, { path: "Scratch.md" })`
);
__vulcanRegisterHelp(
  vault.graph.shortestPath,
  `vault.graph.shortestPath(from: string, to: string): GraphPathReport

Return the shortest resolved path between two notes.

Parameters:
  from - Source note identifier.
  to   - Destination note identifier.

Example:
  vault.graph.shortestPath("Dashboard", "Projects/Beta")

See also: vault.note().neighbors(), vault.graph.hubs()`
);
__vulcanRegisterHelp(
  vault.daily.today,
  `vault.daily.today(): Note

Return today's daily note, enriched with parsed periodic events.

Example:
  vault.daily.today().events

See also: vault.daily.get(), vault.events()`
);
__vulcanRegisterHelp(
  vault.daily.append,
  `vault.daily.append(text: string, opts?: { heading?: string, date?: string }): Note

Append text to a daily note. Requires --sandbox fs or higher.

Parameters:
  text - Markdown text to append.
  opts - Optional heading target and explicit date override.

Example:
  vault.daily.append("- [ ] Review plan", { heading: "Tasks", date: "2026-04-01" })

See also: vault.append(), vault.daily.today()`
);
__vulcanRegisterHelp(
  vault.daily.path,
  `vault.daily.path(date?: string): string

Resolve a daily note path using the vault's periodic-note configuration.

Example:
  vault.daily.path("2026-04-01")`
);
__vulcanRegisterHelp(
  vault.events,
  `vault.events(opts?: { from?: string, to?: string }): DataArray<Event>

Aggregate periodic events across daily notes.

Parameters:
  opts - Optional date window bounds in YYYY-MM-DD form.

Example:
  vault.events({ from: "2026-04-01", to: "2026-04-07" })

See also: vault.daily.range(), vault.daily.today()`
);
__vulcanRegisterHelp(
  vault.set,
  `vault.set(path: string, content: string, opts?: { preserveFrontmatter?: boolean }): Note

Replace note contents. Requires --sandbox fs or higher.

Parameters:
  path    - Target note path.
  content - Complete replacement markdown body.
  opts    - Optional flags such as preserveFrontmatter.

Example:
  vault.set("Scratch", '# Scratch\\n\\nUpdated body')

See also: vault.create(), vault.patch()`
);
__vulcanRegisterHelp(
  vault.create,
  `vault.create(path: string, opts?: string | { content?: string, frontmatter?: object }): Note

Create a new note and return it as a Note object. Requires --sandbox fs or higher.

Parameters:
  path - Target note path.
  opts - The new note's content as a string, or optional content and frontmatter fields.

Example:
  vault.create("Projects/New", { frontmatter: { status: "draft" } })

See also: vault.set(), vault.transaction()`
);
__vulcanRegisterHelp(
  vault.append,
  `vault.append(path: string, text: string, opts?: { heading?: string, prepend?: boolean }): Note

Append or prepend text in one note. Requires --sandbox fs or higher.

Parameters:
  path - Target note path.
  text - Markdown snippet to insert.
  opts - Optional heading target or prepend flag.

Example:
  vault.append("Scratch", "- item", { heading: "Log" })

See also: vault.patch(), vault.daily.append()`
);
__vulcanRegisterHelp(
  vault.patch,
  `vault.patch(path: string, find: string | RegExp, replace: string, opts?: { all?: boolean }): Note

Patch one note. Regex patterns use the JS RegExp source. Requires --sandbox fs or higher.

Parameters:
  path    - Target note path.
  find    - Literal string or JS RegExp.
  replace - Replacement text.
  opts    - Optional all=true for multi-match replacement.

Example:
  vault.patch("Scratch", /TODO/g, "DONE", { all: true })

See also: vault.set(), vault.update()`
);
__vulcanRegisterHelp(
  vault.update,
  `vault.update(path: string, key: string, value: unknown): Note

Set one frontmatter property. Requires --sandbox fs or higher.

Parameters:
  path  - Target note path.
  key   - Frontmatter key.
  value - New serialized property value.

Example:
  vault.update("Scratch", "status", "active")

See also: vault.unset(), vault.transaction()`
);
__vulcanRegisterHelp(
  vault.unset,
  `vault.unset(path: string, key: string): Note

Remove one frontmatter property. Requires --sandbox fs or higher.

Parameters:
  path - Target note path.
  key  - Frontmatter key to remove.

Example:
  vault.unset("Scratch", "status")

See also: vault.update(), vault.transaction()`
);
__vulcanRegisterHelp(
  vault.transaction,
  `vault.transaction(fn: (tx) => unknown): unknown

Run several fs mutations with rollback on JS exceptions.

Parameters:
  fn - Callback that receives transactional mutation helpers.

Example:
  vault.transaction((tx) => { const note = tx.create("Scratch"); tx.append("Index", "- [[" + note.name + "]]"); })

See also: vault.create(), vault.update(), vault.daily.append()`
);
__vulcanRegisterHelp(
  web.search,
  `web.search(query: string, opts?: { limit?: number }): WebSearchReport

Run external web search via the configured backend. Requires --sandbox net or higher.

Parameters:
  query - Search text.
  opts  - Optional result controls such as limit.

Example:
  web.search("Alpha release notes", { limit: 3 })

See also: web.fetch(), vault.search()`
);
__vulcanRegisterHelp(
  web.fetch,
  `web.fetch(url: string, opts?: { mode?: "markdown" | "html" | "raw" }): WebFetchReport

Fetch one URL through Vulcan's web client. Requires --sandbox net or higher.

Parameters:
  url  - Absolute URL to fetch.
  opts - Optional fetch mode.

Example:
  web.fetch("https://example.com", { mode: "markdown" })

See also: web.search(), vault.transaction()`
);

__vulcanRegisterHelp(
  tools.list,
  `tools.list(): Array<CustomTool>

List visible exposed skill command tools with metadata such as description, sandbox, packs, and
callability.

Example:
  tools.list().map((tool) => tool.name)

See also: tools.get(), tools.call()`
);
__vulcanRegisterHelp(
  tools.get,
  `tools.get(name: string): CustomTool

Read one exposed skill command tool definition, including static metadata and the Markdown documentation body.

Parameters:
  name - Tool name, directory name, or manifest path.

Example:
  tools.get("summarize_meeting").body

See also: tools.list(), tools.call()`
);
__vulcanRegisterHelp(
  tools.call,
  `tools.call(name: string, input?: object, opts?: object): unknown

Invoke one exposed skill command tool with validated JSON input.

Parameters:
  name  - Tool name to execute.
  input - Optional JSON-serializable input object; defaults to {}.
  opts  - Reserved for future call options.

Returns:
  The tool's JSON result, or { result, text } when the called tool provided a human fallback.

Notes:
  Nested tool calls preserve the current permission ceiling.
  Recursive tool-call loops are rejected, with a maximum nested depth of 8.

Example:
  tools.call("summarize_meeting", { note: "Meetings/Weekly.md" })

See also: tools.list(), tools.get()`
);
__vulcanRegisterHelp(
  tools.callChecked,
  `tools.callChecked(name: string, input?: object, opts?: object): object

Call a registered tool and attach expect(path) for checked composition.

Example:
  const result = tools.callChecked("task-create", { text: "Call Alex" });
  const id = result.expect("task.id");

See also: tools.call(), skills.run()`
);
__vulcanRegisterHelp(
  vault,
  `vault — Vulcan vault API

Available methods and namespaces:
  vault.note(path)       — resolve one note
  vault.notes(source?)   — query a note collection (DataArray)
  vault.query(dql)       — run a DQL query string, or build one with vault.query()
  vault.search(q, opts?) — full-text search, or build one with vault.search()
  vault.graph.*          — link graph traversal (shortestPath, hubs, deadEnds, etc.)
  vault.daily.*          — periodic/daily note helpers (today, get, range, append)
  vault.plan(opts)       — collect dry-run/apply mutation plans for custom tools
  vault.diffText(a, b)   — produce a small unified diff
  vault.create(path, ...) — create a new note
  vault.set(path, ...)   — overwrite a note
  vault.append(path, ...) — append content to a note
  vault.patch(path, ...)  — patch frontmatter properties
  vault.update(path, ...) — update matching content
  vault.unset(path, ...)  — remove frontmatter properties
  vault.transaction(fn)   — run multiple mutations atomically
  vault.refactor.*        — rename and reorganize helpers

Use help(vault.note), help(vault.search), etc. for details on each method.`
);

__vulcanRegisterHelp(
  vulcan.permissions,
  `vulcan.permissions(): PermissionSummary

Return the active permission profile summary with canRead(path), canWrite(path),
canRefactor(path), and can(capability, resource?) helpers.

Example:
  if (!vulcan.permissions().canWrite("Daily/")) return tool.error("permission_denied", "Cannot write daily notes");`
);
__vulcanRegisterHelp(
  vulcan.date.today,
  `vulcan.date.today(): string

Return today's date string using Vulcan's runtime date policy.

See also: vulcan.date.parse(), vault.daily.path()`
);
__vulcanRegisterHelp(
  tool.result,
  `tool.result(): ToolResultBuilder

Build a standard custom-tool result with summary, changed_paths, diffs, data, or error fields.

Example:
  return tool.result().summary("Updated note").changedPath("Daily.md").data({ count: 1 }).ok();`
);
__vulcanRegisterHelp(
  tool.input,
  `tool.input(defaults?: object): object

Return the validated custom-tool input merged over optional defaults.

Example:
  const input = tool.input({ dry_run: true });`
);
__vulcanRegisterHelp(
  tool.progress,
  `tool.progress(message: string, data?: object): object

Emit a progress message for CLI/MCP-capable hosts and return a structured progress object.

Example:
  tool.progress("Scanning notes", { current: 10, total: 200 });`
);

__vulcanRegisterHelp(
  dv,
  `dv — Dataview-compatible JS API

  dv.current()             — metadata for the current note
  dv.page(path)            — single page metadata object
  dv.pages(source?)        — DataArray of all pages (filterable)
  dv.table(headers, rows)  — render a table output
  dv.list(items)           — render a bullet list
  dv.taskList(tasks, group?) — render a task list
  dv.paragraph(text)       — render a text paragraph
  dv.header(level, text)   — render a heading (level 1-6)
  dv.span(text)            — render inline text
  dv.el(tag, text, attrs?) — render an arbitrary HTML element
  dv.pagePaths(source?)    — DataArray of matching page paths
  await dv.query(dql)      — run DQL, resolving to { successful, value | error } with
                             Dataview's { type, values, headers? } result
  await dv.tryQuery(dql)   — run DQL, resolving to the result or rejecting
  await dv.queryMarkdown(dql) — run DQL, resolving to rendered Markdown
  dv.execute(dql)          — run a DQL query and emit its output
  dv.evaluate(expr, ctx?)  — evaluate a Dataview expression ({ successful, value })
  dv.tryEvaluate(expr, ctx?) — evaluate a Dataview expression, throwing on failure
  dv.markdownTable(headers, rows) — render rows as Markdown table text
  dv.markdownList(values)  — render values as Markdown list text
  await dv.io.csv(path)    — load a CSV file as DataArray
  await dv.io.load(path)   — load a file as raw text
  dv.func.*                — DataviewJS utility functions (date, duration, etc.)
  dv.luxon.*               — Luxon DateTime and Duration classes

Use dv.pages('#tag') or dv.pages('"Folder"') to filter.`
);

__vulcanRegisterHelp(
  tools,
  `tools — Exposed skill command tool registry

  tools.list()             — list visible skill command tools
  tools.get(name)          — read one tool definition plus documentation body
  tools.call(name, input?) — invoke one skill command tool with validated JSON input
  tools.callChecked(name, input?) — call a tool and assert expected result paths

Use help(tools.call) for execution details and nested-call limits.`
);

__vulcanRegisterHelp(
  skills,
  `skills — Agent Skills-compatible command registry

  skills.list()                    — list projected skill-command tools
  skills.get(toolName)             — read one projected skill-command tool definition
  skills.commands(skillName?)      — list commands, optionally scoped to one skill
  skills.run(skill, command, input?) — invoke a skill command via the shared tool registry

Skill commands are permission-checked like tools.call() and use names such as
skill_daily_review_prepare_day on tool surfaces.`
);

__vulcanRegisterHelp(
  host.exec,
  `host.exec(argv: string[], opts?: { cwd?: string, env?: object, timeout_ms?: number, max_output_bytes?: number }): HostProcessReport

Run one local command without shell parsing.

Parameters:
  argv             - Explicit argument vector. argv[0] is the program to execute.
  opts.cwd         - Optional working directory relative to the current script file or vault root.
  opts.env         - Optional environment overrides. null removes one inherited variable.
  opts.timeout_ms  - Optional tighter timeout cap for this subprocess.
  opts.max_output_bytes - Optional per-stream stdout/stderr capture cap.

Returns:
  A structured process report with success, exit_code, stdout, stderr, truncation flags, and
  duration_ms.

Permissions:
  Requires execute access under the active permission profile.
  The effective timeout is capped by the surrounding JS runtime timeout.

Example:
  host.exec(["git", "status", "--short"], { cwd: "." })

See also: host.shell(), tools.call()`
);
__vulcanRegisterHelp(
  host.shell,
  `host.shell(command: string, opts?: { cwd?: string, env?: object, timeout_ms?: number, max_output_bytes?: number }): HostProcessReport

Run one local command string through the configured shell.

Parameters:
  command          - Shell command string.
  opts.cwd         - Optional working directory relative to the current script file or vault root.
  opts.env         - Optional environment overrides. null removes one inherited variable.
  opts.timeout_ms  - Optional tighter timeout cap for this subprocess.
  opts.max_output_bytes - Optional per-stream stdout/stderr capture cap.

Permissions:
  Requires shell access under the active permission profile.
  This is higher risk than host.exec() because shell parsing and expansion are involved.

Example:
  host.shell("git status --short", { cwd: "." })

See also: host.exec(), help(host)`
);
__vulcanRegisterHelp(
  host,
  `host — Permission-gated host process execution

  host.exec(argv, opts?)     — execute one argument vector without a shell
  host.shell(command, opts?) — execute one shell command string

Prefer host.exec() whenever argument-vector execution is sufficient.
Use help(host.exec) or help(host.shell) for timeout, cwd, and output details.`
);

__vulcanRegisterHelp(
  web,
  `web — External web access (requires --sandbox net)

  web.search(query, opts?)  — search using the configured web backend
  web.fetch(url, opts?)     — fetch and convert a URL to markdown/html/raw

Use help(web.search) or help(web.fetch) for parameter details.`
);

__vulcanRegisterHelp(
  console,
  `console — REPL output helpers

  console.log(...args)   — print space-joined text output`
);

// Obsidian API compatibility stub
const app = {
  vault: {
    getName() {
      return __vulcan_vault_name_json();
    },
    getMarkdownFiles() {
      return JSON.parse(__vulcan_pages_json(null)).map((page) => ({
        path: page.file?.path ?? page.path ?? "",
        name: page.file?.name ?? page.name ?? "",
        basename: page.file?.name ?? page.name ?? "",
        extension: "md",
        stat: { mtime: __vulcanDateMillis(page.file?.mtime) ?? 0, ctime: 0, size: 0 },
      }));
    },
    read(file) {
      const path = typeof file === "string" ? file : (file?.path ?? String(file));
      return vault.note(path).content;
    },
    modify(file, content) {
      const path = typeof file === "string" ? file : (file?.path ?? String(file));
      vault.set(path, { content });
    },
    getAbstractFileByPath(path) {
      try {
        const note = vault.note(path);
        if (note) {
          return { path: note.path, name: note.file?.name ?? path, basename: note.file?.name ?? path, extension: "md" };
        }
        return null;
      } catch (_) {
        return null;
      }
    },
  },
  workspace: new Proxy({}, {
    get(_, prop) {
      throw new Error(
        "app.workspace is not supported in Vulcan — use vault.* instead. Accessed: app.workspace." + String(prop)
      );
    },
  }),
  metadataCache: new Proxy({}, {
    get(_, prop) {
      throw new Error(
        "app.metadataCache is not supported in Vulcan — use vault.query() or vault.notes() instead. Accessed: app.metadataCache." + String(prop)
      );
    },
  }),
};

__vulcanRegisterHelp(
  app,
  `app — Obsidian API compatibility stub

  app.vault.getName()                    — vault root folder name
  app.vault.getMarkdownFiles()           — all notes as TFile-like objects
  app.vault.read(file)                   — read note content (path string or TFile)
  app.vault.modify(file, content)        — overwrite note content
  app.vault.getAbstractFileByPath(path)  — resolve path to TFile-like object

Note: app.workspace and app.metadataCache throw descriptive errors.
Prefer vault.* over app.vault.* for full Vulcan-native functionality.`
);

globalThis.dv = dv;
globalThis.vault = vault;
globalThis.tools = tools;
globalThis.skills = skills;
globalThis.host = host;
globalThis.web = web;
globalThis.console = console;
globalThis.help = help;
globalThis.app = app;
globalThis._ = undefined;
globalThis._error = undefined;
globalThis.this = dv.current();
"##;

    pub fn evaluate_dataview_js(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        evaluate_dataview_js_with_options(
            paths,
            source,
            current_file,
            DataviewJsEvalOptions::default(),
        )
    }

    pub fn evaluate_dataview_js_with_options(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
        options: DataviewJsEvalOptions,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        DataviewJsSession::new(paths, current_file, options)?.evaluate(source)
    }

    pub fn evaluate_dataview_js_query(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        evaluate_dataview_js(paths, source, current_file)
    }

    impl DataviewJsSession {
        #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
        pub fn new(
            paths: &VaultPaths,
            current_file: Option<&str>,
            options: DataviewJsEvalOptions,
        ) -> Result<Self, DataviewJsError> {
            let loaded_config = load_vault_config(paths).config;
            if !loaded_config.dataview.enable_dataview_js {
                return Err(DataviewJsError::Disabled);
            }

            let permission_selection = if let Some(selection) = options.resolved_permissions.clone()
            {
                Some(selection)
            } else {
                options
                    .permission_profile
                    .as_deref()
                    .map(|profile| resolve_permission_profile(paths, Some(profile)))
                    .transpose()
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?
            };
            let permissions = permission_selection.map(|selection| {
                if options.disable_policy_hooks {
                    ProfilePermissionGuard::without_policy_hooks(paths, selection)
                } else {
                    ProfilePermissionGuard::new(paths, selection)
                }
            });
            let note_index = load_scoped_note_index(paths, permissions.as_ref())?;
            let sandbox = options
                .sandbox
                .unwrap_or(loaded_config.js_runtime.default_sandbox);
            // `execute` governs host processes (design document, "Host
            // process execution"). The sandboxed tiers reach the vault only
            // through profile-checked APIs and run under the profile's
            // JavaScript limits; only `none`, which lifts those limits and
            // can start processes, needs it up front.
            if sandbox == JsRuntimeSandbox::None {
                if let Some(permissions) = permissions.as_ref() {
                    permissions
                        .check_execute()
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                }
            }
            let timeout = effective_timeout(
                &loaded_config,
                sandbox,
                options.timeout,
                permissions.as_ref(),
            )?;
            if timeout.is_some_and(|timeout| timeout.is_zero()) {
                return Err(DataviewJsError::Message(
                    "DataviewJS timeout must be greater than 0ms".to_string(),
                ));
            }
            let state = Arc::new(JsEvalState {
                paths: paths.clone(),
                current_file: current_file.map(ToOwned::to_owned),
                note_index: Mutex::new(JsNoteIndex::new(note_index, 0)),
                retired_note_indexes: Mutex::new(Vec::new()),
                periodic_config: loaded_config.periodic.clone(),
                inbox_config: loaded_config.inbox.clone(),
                web_config: loaded_config.web.clone(),
                shell_path: loaded_config.templates.shell_path.clone(),
                sandbox,
                permissions,
                deterministic_static: options.deterministic_static,
                read_only: options.read_only,
                link_resolution: loaded_config.link_resolution,
                runtime_timeout: timeout,
                transaction: Mutex::new(None),
                tool_registry: options.tool_registry,
                mutation_committer: options.mutation_committer,
                granted_host_commands: options.granted_host_commands,
            });
            let outputs = Arc::new(Mutex::new(Vec::new()));
            let runtime =
                Runtime::new().map_err(|error| DataviewJsError::Message(error.to_string()))?;
            if sandbox_uses_resource_limits(sandbox) {
                runtime.set_memory_limit(runtime_memory_limit_bytes(
                    &loaded_config.js_runtime,
                    state.permissions.as_ref(),
                ));
                runtime.set_max_stack_size(runtime_stack_limit_bytes(
                    &loaded_config.js_runtime,
                    state.permissions.as_ref(),
                ));
            }
            let timeout_description =
                timeout.map_or_else(|| "no limit".to_string(), format_timeout);

            let context = Context::full(&runtime)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            runtime.set_host_promise_rejection_tracker(Some(Box::new(
                |ctx, promise, reason, handled| {
                    if let Ok(track) = ctx
                        .globals()
                        .get::<_, rquickjs::Function>("__vulcanTrackRejection")
                    {
                        let _ = track.call::<_, ()>((promise, reason, handled));
                    }
                },
            )));
            context.with(|ctx| -> Result<(), DataviewJsError> {
                install_dataview_globals(ctx.clone(), Arc::clone(&state), Arc::clone(&outputs))?;
                ctx.globals()
                    .set(
                        "__vulcan_dataview_settings_json",
                        serde_json::json!({
                            "defaultDateFormat": loaded_config.dataview.default_date_format,
                            "defaultDateTimeFormat":
                                loaded_config.dataview.default_datetime_format,
                        })
                        .to_string(),
                    )
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                ctx.eval::<(), _>(DATAVIEW_JS_PRELUDE)
                    .catch(&ctx)
                    .map_err(|error| map_caught_runtime_error(&error, &timeout_description))?;
                if loaded_config.customjs.is_configured() {
                    install_customjs(
                        &ctx,
                        &state,
                        loaded_config.customjs.clone(),
                        &timeout_description,
                    )?;
                }
                Ok(())
            })?;

            Ok(Self {
                runtime,
                context,
                outputs,
                timeout,
            })
        }

        pub fn evaluate(&self, source: &str) -> Result<DataviewJsResult, DataviewJsError> {
            self.evaluate_with_mode(source, true)
        }

        /// Evaluate `source` as non-strict global code, as Dataview runs a
        /// note's scripts: assigning an undeclared name (`test = 3`) creates
        /// a global that later evaluations in this session can read.
        pub fn evaluate_sloppy(&self, source: &str) -> Result<DataviewJsResult, DataviewJsError> {
            self.evaluate_with_mode(source, false)
        }

        /// Evaluate a `dataviewjs` block non-strictly inside its own braces:
        /// its `const`, `let`, and `class` declarations stay local, as in
        /// Dataview, where each block is a separate function, while plain
        /// assignments (`fmt = (x) => …`) reach the session's shared scope.
        ///
        /// A block that uses `await` runs as an async function, as Dataview
        /// runs every block, so `await cJS()` and `await dv.io.load()` work;
        /// a rejection is rethrown once the session has drained its jobs.
        pub fn evaluate_block(&self, source: &str) -> Result<DataviewJsResult, DataviewJsError> {
            // The opening shares the first line so error line numbers match.
            if !uses_await(source) {
                return self.evaluate_with_mode(&format!("{{{source}\n}}"), false);
            }
            let mut result = self.evaluate_with_mode(
                &format!(
                    "globalThis.__vulcanBlockError = undefined; (async () => {{{source}\n}})()\
                     .catch((error) => {{ globalThis.__vulcanBlockError = error; }}); undefined"
                ),
                false,
            )?;
            let settled = self.evaluate_with_mode(
                "if (globalThis.__vulcanBlockError !== undefined) { \
                 const error = globalThis.__vulcanBlockError; \
                 globalThis.__vulcanBlockError = undefined; throw error; }",
                false,
            )?;
            result.outputs.extend(settled.outputs);
            Ok(result)
        }

        /// Run `body` as the body of an async function and return what it
        /// returned (awaited), as JSON; `None` for `undefined`. Hosts that
        /// run scripts with their own globals, such as Meta Bind JS views
        /// and buttons, declare them at the top of `body`.
        pub fn evaluate_async_function(
            &self,
            body: &str,
        ) -> Result<Option<Value>, DataviewJsError> {
            self.evaluate_with_mode(
                &format!(
                    "globalThis.__vulcanFnResult = undefined; globalThis.__vulcanFnError = undefined; \
                     (async () => {{{body}\n}})().then(\
                     (value) => {{ globalThis.__vulcanFnResult = {{ value }}; }}, \
                     (error) => {{ globalThis.__vulcanFnError = error; }}); undefined"
                ),
                false,
            )?;
            Ok(self
                .evaluate_with_mode(
                    "(() => { const error = globalThis.__vulcanFnError; \
                     const result = globalThis.__vulcanFnResult; \
                     globalThis.__vulcanFnError = undefined; \
                     globalThis.__vulcanFnResult = undefined; \
                     if (error !== undefined) throw error; \
                     return result === undefined ? undefined : result.value; })()",
                    false,
                )?
                .value)
        }

        fn evaluate_with_mode(
            &self,
            source: &str,
            strict: bool,
        ) -> Result<DataviewJsResult, DataviewJsError> {
            {
                let mut outputs = self.outputs.lock().map_err(|_| {
                    DataviewJsError::Message("DataviewJS output lock poisoned".to_string())
                })?;
                outputs.clear();
            }

            let timeout_description = self
                .timeout
                .map_or_else(|| "no limit".to_string(), format_timeout);
            if let Some(timeout) = self.timeout {
                let deadline = Instant::now();
                self.runtime
                    .set_interrupt_handler(Some(Box::new(move || deadline.elapsed() >= timeout)));
            } else {
                self.runtime.set_interrupt_handler(None);
            }

            let eval_result = self
                .context
                .with(|ctx| evaluate_value(&ctx, source, strict, &timeout_description));
            let drain_result = drain_pending_jobs(&self.runtime, &timeout_description);
            self.runtime.set_interrupt_handler(None);
            drain_result?;
            let value = match eval_result? {
                EvaluatedValue::Ready(value) => value,
                EvaluatedValue::Awaiting => self
                    .context
                    .with(|ctx| take_awaited_value(&ctx, &timeout_description))?,
            };
            // A promise rejected with no handler fails the evaluation, as an
            // uncaught exception would, instead of vanishing.
            self.context.with(|ctx| {
                ctx.eval::<(), _>("__vulcanThrowUnhandledRejection()")
                    .catch(&ctx)
                    .map_err(|error| map_caught_runtime_error(&error, &timeout_description))
            })?;
            let outputs = self.outputs.lock().map_err(|_| {
                DataviewJsError::Message("DataviewJS output lock poisoned".to_string())
            })?;

            Ok(DataviewJsResult {
                outputs: outputs.clone(),
                value,
            })
        }
    }

    /// Define `customJS`, `cJS()`, and `forceLoadCustomJS()`. The class
    /// files are read, through the session's read permissions, only when a
    /// script first uses them.
    fn install_customjs(
        ctx: &Ctx<'_>,
        state: &Arc<JsEvalState>,
        config: crate::config::CustomJsConfig,
        timeout_description: &str,
    ) -> Result<(), DataviewJsError> {
        let sources_state = Arc::clone(state);
        ctx.globals()
            .set(
                "__vulcan_customjs_sources",
                Func::from(move |ctx: Ctx<'_>| {
                    let guard = sources_state
                        .permissions
                        .as_ref()
                        .map(|guard| guard as &dyn crate::permissions::PermissionGuard);
                    let sources = crate::customjs::load_customjs_sources(
                        &sources_state.paths,
                        &config,
                        guard,
                    );
                    to_json_string(&ctx, sources)
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        ctx.eval::<(), _>(CUSTOMJS_PRELUDE)
            .catch(ctx)
            .map_err(|error| map_caught_runtime_error(&error, timeout_description))
    }

    use crate::customjs::CUSTOMJS_PRELUDE;

    /// Whether `source` contains `await` as a word, so its block needs an
    /// async function. A mention in a string or comment only costs the
    /// block its completion value, which blocks with output never show.
    fn uses_await(source: &str) -> bool {
        source.match_indices("await").any(|(index, _)| {
            let word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
            let before = source[..index].chars().next_back();
            let after = source[index + "await".len()..].chars().next();
            !before.is_some_and(word_char) && !after.is_some_and(word_char)
        })
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    fn install_dataview_globals(
        ctx: Ctx<'_>,
        state: Arc<JsEvalState>,
        outputs: Arc<Mutex<Vec<DataviewJsOutput>>>,
    ) -> Result<(), DataviewJsError> {
        let globals = ctx.globals();

        globals
            .set("__vulcan_deterministic_static", state.deterministic_static)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        globals
            .set("__vulcan_rendering", state.read_only)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        globals
            .set("__vulcan_current_file", state.current_file.clone())
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_emit",
                Func::from(move |ctx: Ctx<'_>, json: String| {
                    let output: DataviewJsOutput = parse_json_string(&ctx, &json)?;
                    outputs
                        .lock()
                        .map_err(|_| {
                            Exception::throw_message(&ctx, "DataviewJS output lock poisoned")
                        })?
                        .push(output);
                    Ok::<(), rquickjs::Error>(())
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let pages_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_pages_json",
                Func::from(move |ctx: Ctx<'_>, source: Option<String>| {
                    with_note_index(
                        &pages_state,
                        &ctx,
                        |note_index| select_pages(&pages_state, note_index, source.as_deref()),
                        |note_index, selected| {
                            to_json_string(
                                &ctx,
                                pages_for_paths(
                                    note_index,
                                    &selected,
                                    Some(pages_state.paths.vault_root()),
                                ),
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let lazy_pages_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_lazy_pages_json",
                Func::from(move |ctx: Ctx<'_>, source: Option<String>| {
                    let mut note_index = lazy_pages_state.note_index.lock().map_err(|_| {
                        Exception::throw_message(&ctx, "DataviewJS note index lock poisoned")
                    })?;
                    let (_, selected) =
                        select_pages(&lazy_pages_state, &note_index, source.as_deref())
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    let pages = lazy_pages_for_paths(&note_index, &selected);
                    note_index.page_sets.push(selected);
                    to_json_string(
                        &ctx,
                        serde_json::json!({
                            "generation": note_index.generation,
                            "set": note_index.page_sets.len() - 1,
                            "pages": pages,
                        }),
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let page_field_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_page_file_field_json",
                Func::from(
                    move |ctx: Ctx<'_>,
                          generation: usize,
                          set: usize,
                          path: String,
                          key: String| {
                        read_lazy_page_field(&page_field_state, generation, set, &path, &key)
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                            .and_then(|value| to_json_string(&ctx, value))
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let single_page_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_page_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    with_note_index(
                        &single_page_state,
                        &ctx,
                        |_| {
                            let resolved = resolve_page_path(&single_page_state.paths, &path)?;
                            Ok((IndexNeed::Paths(vec![resolved.clone()]), resolved))
                        },
                        |note_index, resolved| {
                            to_json_string(
                                &ctx,
                                note_index.note(&resolved).map_or(Value::Null, |note| {
                                    page_object(note, Some(single_page_state.paths.vault_root()))
                                }),
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let current_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_current_json",
                Func::from(move |ctx: Ctx<'_>| {
                    with_note_index(
                        &current_state,
                        &ctx,
                        |_| {
                            Ok((
                                IndexNeed::Paths(
                                    current_state.current_file.iter().cloned().collect(),
                                ),
                                (),
                            ))
                        },
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                current_state
                                    .current_file
                                    .as_deref()
                                    .map(|path| {
                                        page_object_by_reference(
                                            &current_state.paths,
                                            note_index,
                                            path,
                                        )
                                    })
                                    .transpose()
                                    .map_err(|error| {
                                        Exception::throw_message(&ctx, &error.to_string())
                                    })?
                                    .unwrap_or(Value::Null),
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_details_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_details_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    with_note_index(
                        &note_details_state,
                        &ctx,
                        |_| Ok((IndexNeed::All, ())),
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                load_note_details(&note_details_state, note_index, &path).map_err(
                                    |error| Exception::throw_message(&ctx, &error.to_string()),
                                )?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_read_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_read_json",
                Func::from(move |ctx: Ctx<'_>, path: String, options_json: String| {
                    let options = parse_json_string::<crate::NoteReadOptions>(&ctx, &options_json)?;
                    with_note_index(
                        &note_read_state,
                        &ctx,
                        |_| Ok((IndexNeed::Nothing, ())),
                        |_note_index, ()| {
                            to_json_string(
                                &ctx,
                                read_note_selection(&note_read_state, &path, &options).map_err(
                                    |error| Exception::throw_message(&ctx, &error.to_string()),
                                )?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_links_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_links_json",
                Func::from(move |ctx: Ctx<'_>, path: String, direction: String| {
                    with_note_index(
                        &note_links_state,
                        &ctx,
                        |_| Ok((IndexNeed::All, ())),
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                load_note_relationships(
                                    &note_links_state.paths,
                                    note_index,
                                    &path,
                                    &direction,
                                    note_links_state.permissions.as_ref(),
                                )
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_neighbors_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_neighbors_json",
                Func::from(move |ctx: Ctx<'_>, path: String, depth: i32| {
                    with_note_index(
                        &note_neighbors_state,
                        &ctx,
                        |_| Ok((IndexNeed::All, ())),
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                load_note_neighbors(
                                    &note_neighbors_state.paths,
                                    note_index,
                                    &path,
                                    depth,
                                    note_neighbors_state.permissions.as_ref(),
                                )
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let shorthand_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_date_shorthand_millis",
                Func::from(move |ctx: Ctx<'_>, name: String| {
                    let Some(millis) = crate::expression::functions::date_shortcut_value(
                        &name,
                        crate::current_utc_timestamp_ms(),
                    ) else {
                        return Ok(None);
                    };
                    if shorthand_state.deterministic_static {
                        return Err(Exception::throw_message(
                            &ctx,
                            &static_mode_time_denied(&format!("dv.date(\"{name}\")")).to_string(),
                        ));
                    }
                    Ok::<Option<i64>, rquickjs::Error>(Some(millis))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let today_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_today",
                Func::from(move |ctx: Ctx<'_>| {
                    current_today(&today_state, "vault.daily.today()")
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_daily_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_daily_json",
                Func::from(move |ctx: Ctx<'_>, date: String| {
                    with_note_index(
                        &vault_daily_state,
                        &ctx,
                        |_| Ok((IndexNeed::All, ())),
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                load_daily_page_object(
                                    &vault_daily_state.paths,
                                    &vault_daily_state.periodic_config,
                                    note_index,
                                    &date,
                                    vault_daily_state.deterministic_static,
                                    "vault.daily.get()",
                                )
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let daily_path_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_daily_path",
                Func::from(move |ctx: Ctx<'_>, date: String| {
                    let date = normalize_daily_event_date(
                        Some(&date),
                        daily_path_state.deterministic_static,
                        "vault.daily.path()",
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    crate::expected_periodic_note_path(
                        &daily_path_state.periodic_config,
                        "daily",
                        &date,
                    )
                    .ok_or_else(|| {
                        Exception::throw_message(
                            &ctx,
                            &format!("failed to resolve daily note path for {date}"),
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_daily_range_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_daily_range_json",
                Func::from(move |ctx: Ctx<'_>, from: String, to: String| {
                    with_note_index(
                        &vault_daily_range_state,
                        &ctx,
                        |_| Ok((IndexNeed::All, ())),
                        |note_index, ()| {
                            let note_index = &note_index.notes;
                            to_json_string(
                                &ctx,
                                load_daily_range_objects(
                                    &vault_daily_range_state.paths,
                                    note_index,
                                    &from,
                                    &to,
                                    vault_daily_range_state.deterministic_static,
                                    "vault.daily.range()",
                                )
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?,
                            )
                        },
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_events_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_events_json",
                Func::from(
                    move |ctx: Ctx<'_>, from: Option<String>, to: Option<String>| {
                        let (from, to) = normalize_daily_event_range(
                            from.as_deref(),
                            to.as_deref(),
                            vault_events_state.deterministic_static,
                            "vault.events()",
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                        to_json_string(
                            &ctx,
                            list_events_between(&vault_events_state.paths, &from, &to).map_err(
                                |error| Exception::throw_message(&ctx, &error.to_string()),
                            )?,
                        )
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let query_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_query_json",
                Func::from(move |ctx: Ctx<'_>, dql: String, file: Option<String>| {
                    let current_file = resolved_current_file(
                        &query_state.paths,
                        file.as_deref(),
                        query_state.current_file.as_deref(),
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    let response =
                        match evaluate_dql(&query_state.paths, &dql, current_file.as_deref()) {
                            Ok(value) => QueryResponse {
                                successful: true,
                                value: Some(value),
                                error: None,
                            },
                            Err(error) => QueryResponse {
                                successful: false,
                                value: None,
                                error: Some(error.to_string()),
                            },
                        };
                    to_json_string(&ctx, response)
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let resolve_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_resolve_link_path",
                Func::from(
                    move |ctx: Ctx<'_>, target: String, origin: Option<String>| {
                        let mut note_index = resolve_state.note_index.lock().map_err(|_| {
                            Exception::throw_message(&ctx, "DataviewJS note index lock poisoned")
                        })?;
                        Ok::<_, rquickjs::Error>(note_index.resolve_link_path(
                            &target,
                            origin.as_deref(),
                            resolve_state.link_resolution,
                        ))
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        // dv.evaluate(): a Dataview expression whose fields come from the
        // context object and whose `this` is the current page.
        let evaluate_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_evaluate_json",
                Func::from(
                    move |ctx: Ctx<'_>, expression: String, context_json: Option<String>| {
                        let fields = match context_json
                            .as_deref()
                            .map(serde_json::from_str::<Value>)
                            .transpose()
                        {
                            Ok(Some(Value::Object(fields))) => fields,
                            Ok(_) => Map::new(),
                            Err(error) => {
                                return Err(Exception::throw_message(&ctx, &error.to_string()))
                            }
                        };
                        with_note_index(
                            &evaluate_state,
                            &ctx,
                            |_| {
                                Ok((
                                    IndexNeed::Paths(
                                        evaluate_state.current_file.iter().cloned().collect(),
                                    ),
                                    (),
                                ))
                            },
                            |note_index, ()| {
                                let notes = &note_index.notes;
                                let row = crate::dql::eval::synthetic_group_note(&fields);
                                let formulas = std::collections::BTreeMap::new();
                                let this_note = evaluate_state
                                    .current_file
                                    .as_deref()
                                    .and_then(|path| note_index.note(path));
                                let mut eval_ctx = crate::expression::eval::EvalContext::new(
                                    &row, &formulas,
                                )
                                .with_note_lookup(notes)
                                .with_this_null_when_missing();
                                if let Some(this_note) = this_note {
                                    eval_ctx = eval_ctx.with_this_note(this_note);
                                }
                                let result = crate::expression::parse_expression(&expression)
                                    .map_err(|error| {
                                        format!("Failed to parse expression \"{expression}\": {error}")
                                    })
                                    .and_then(|expr| {
                                        crate::expression::eval::evaluate(&expr, &eval_ctx)
                                    });
                                to_json_string(
                                    &ctx,
                                    match result {
                                        Ok(value) => {
                                            serde_json::json!({ "successful": true, "value": value })
                                        }
                                        Err(error) => {
                                            serde_json::json!({ "successful": false, "error": error })
                                        }
                                    },
                                )
                            },
                        )
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let search_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_search_json",
                Func::from(move |ctx: Ctx<'_>, query: String, limit: Option<usize>| {
                    let read_filter = search_state
                        .permissions
                        .as_ref()
                        .map(PermissionGuard::read_filter);
                    to_json_string(
                        &ctx,
                        search_vault_with_filter(
                            &search_state.paths,
                            &SearchQuery {
                                text: query,
                                limit,
                                ..SearchQuery::default()
                            },
                            read_filter.as_ref(),
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let graph_path_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_graph_path_json",
                Func::from(move |ctx: Ctx<'_>, from: String, to: String| {
                    let read_filter = graph_path_state
                        .permissions
                        .as_ref()
                        .map(PermissionGuard::read_filter);
                    to_json_string(
                        &ctx,
                        query_graph_path_with_filter(
                            &graph_path_state.paths,
                            &from,
                            &to,
                            read_filter.as_ref(),
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let graph_hubs_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_graph_hubs_json",
                Func::from(move |ctx: Ctx<'_>, limit: Option<usize>| {
                    let read_filter = graph_hubs_state
                        .permissions
                        .as_ref()
                        .map(PermissionGuard::read_filter);
                    let mut report =
                        query_graph_hubs_with_filter(&graph_hubs_state.paths, read_filter.as_ref())
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    if let Some(limit) = limit {
                        report.notes.truncate(limit);
                    }
                    to_json_string(&ctx, report)
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let graph_components_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_graph_components_json",
                Func::from(move |ctx: Ctx<'_>, limit: Option<usize>| {
                    let read_filter = graph_components_state
                        .permissions
                        .as_ref()
                        .map(PermissionGuard::read_filter);
                    let mut report = query_graph_components_with_filter(
                        &graph_components_state.paths,
                        read_filter.as_ref(),
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    if let Some(limit) = limit {
                        report.components.truncate(limit);
                    }
                    to_json_string(&ctx, report)
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let graph_dead_ends_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_graph_dead_ends_json",
                Func::from(move |ctx: Ctx<'_>, limit: Option<usize>| {
                    let read_filter = graph_dead_ends_state
                        .permissions
                        .as_ref()
                        .map(PermissionGuard::read_filter);
                    let mut report = query_graph_dead_ends_with_filter(
                        &graph_dead_ends_state.paths,
                        read_filter.as_ref(),
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?;
                    if let Some(limit) = limit {
                        report.notes.truncate(limit);
                    }
                    to_json_string(&ctx, report)
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let load_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_load",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    read_text_file(
                        &load_state,
                        &path,
                        load_state.current_file.as_deref(),
                        "dv.io.load()",
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let csv_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_csv_json",
                Func::from(
                    move |ctx: Ctx<'_>, path: String, origin_file: Option<String>| {
                        to_json_string(
                            &ctx,
                            read_csv_file(
                                &csv_state,
                                &path,
                                origin_file.as_deref().or(csv_state.current_file.as_deref()),
                            )
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let normalize_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_normalize",
                Func::from(
                    move |ctx: Ctx<'_>, path: String, origin_file: Option<String>| {
                        normalize_vault_path(
                            normalize_state.paths.vault_root(),
                            &path,
                            origin_file
                                .as_deref()
                                .or(normalize_state.current_file.as_deref()),
                            false,
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let view_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_load_view",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    read_text_file(
                        &view_state,
                        &path,
                        view_state.current_file.as_deref(),
                        "dv.view()",
                    )
                    .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_compare",
                Func::from(move |ctx: Ctx<'_>, left_json: String, right_json: String| {
                    let left_json: Value = parse_json_string(&ctx, &left_json)?;
                    let right_json: Value = parse_json_string(&ctx, &right_json)?;
                    Ok::<i32, rquickjs::Error>(compare_json_values(&left_json, &right_json))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_equal",
                Func::from(move |ctx: Ctx<'_>, left_json: String, right_json: String| {
                    let left_json: Value = parse_json_string(&ctx, &left_json)?;
                    let right_json: Value = parse_json_string(&ctx, &right_json)?;
                    Ok::<bool, rquickjs::Error>(group_keys_equal(&left_json, &right_json))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_date_millis",
                Func::from(move |ctx: Ctx<'_>, input: String| {
                    let _ = ctx;
                    Ok::<Option<i64>, rquickjs::Error>(parse_date_like_string(&input))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_duration_millis",
                Func::from(move |ctx: Ctx<'_>, input: String| {
                    let _ = ctx;
                    Ok::<Option<i64>, rquickjs::Error>(parse_duration_string(&input))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_format_date",
                Func::from(move |ctx: Ctx<'_>, millis: i64, format: String| {
                    let _ = ctx;
                    Ok::<String, rquickjs::Error>(format_date(millis, &format))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_format_duration",
                Func::from(move |ctx: Ctx<'_>, millis: i64, format: String| {
                    let _ = ctx;
                    Ok::<String, rquickjs::Error>(format_duration(millis, &format))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        globals
            .set(
                "__vulcan_link_meta_json",
                Func::from(move |ctx: Ctx<'_>, value_json: String| {
                    let value: Value = parse_json_string(&ctx, &value_json)?;
                    to_json_string(&ctx, link_meta_value(&value))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let mutation_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_mutate_json",
                Func::from(move |ctx: Ctx<'_>, kind: String, payload_json: String| {
                    let payload: Value = parse_json_string(&ctx, &payload_json)?;
                    to_json_string(
                        &ctx,
                        apply_js_mutation(&mutation_state, &kind, payload)
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let permissions_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_permissions_json",
                Func::from(move |ctx: Ctx<'_>| {
                    to_json_string(&ctx, js_permissions_summary(&permissions_state))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let permission_check_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_permission_check_json",
                Func::from(move |ctx: Ctx<'_>, action: String, path: Option<String>| {
                    to_json_string(
                        &ctx,
                        js_permission_check(&permission_check_state, &action, path.as_deref()),
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tx_begin_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_tx_begin",
                Func::from(move |ctx: Ctx<'_>| {
                    begin_transaction(&tx_begin_state)
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tx_commit_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_tx_commit",
                Func::from(move |ctx: Ctx<'_>| {
                    commit_transaction(&tx_commit_state)
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tx_rollback_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_tx_rollback",
                Func::from(move |ctx: Ctx<'_>| {
                    rollback_transaction(&tx_rollback_state)
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let web_search_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_web_search_json",
                Func::from(move |ctx: Ctx<'_>, query: String, limit: Option<usize>| {
                    to_json_string(
                        &ctx,
                        run_js_web_search(&web_search_state, &query, limit.unwrap_or(10))
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let web_fetch_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_web_fetch_json",
                Func::from(move |ctx: Ctx<'_>, url: String, mode: String| {
                    to_json_string(
                        &ctx,
                        run_js_web_fetch(&web_fetch_state, &url, &mode)
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_name_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_name_json",
                Func::from(move || {
                    vault_name_state
                        .paths
                        .vault_root()
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string()
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tools_list_registry = state.tool_registry.clone();
        globals
            .set(
                "__vulcan_tools_list_json",
                Func::from(move |ctx: Ctx<'_>| {
                    let Some(registry) = tools_list_registry.as_ref() else {
                        return to_json_string(&ctx, Vec::<DataviewJsToolDescriptor>::new());
                    };
                    to_json_string(
                        &ctx,
                        registry
                            .list()
                            .map_err(|error| Exception::throw_message(&ctx, &error))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tools_get_registry = state.tool_registry.clone();
        globals
            .set(
                "__vulcan_tools_get_json",
                Func::from(move |ctx: Ctx<'_>, name: String| {
                    let Some(registry) = tools_get_registry.as_ref() else {
                        return Err(Exception::throw_message(
                            &ctx,
                            "tool registry is not available in this runtime",
                        ));
                    };
                    to_json_string(
                        &ctx,
                        registry
                            .get(&name)
                            .map_err(|error| Exception::throw_message(&ctx, &error))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let tools_call_registry = state.tool_registry.clone();
        globals
            .set(
                "__vulcan_tools_call_json",
                Func::from(
                    move |ctx: Ctx<'_>, name: String, input_json: String, options_json: String| {
                        let Some(registry) = tools_call_registry.as_ref() else {
                            return Err(Exception::throw_message(
                                &ctx,
                                "tool registry is not available in this runtime",
                            ));
                        };
                        let input = parse_json_string::<Value>(&ctx, &input_json)?;
                        let options = parse_json_string::<Option<Value>>(&ctx, &options_json)?;
                        to_json_string(
                            &ctx,
                            registry
                                .call(&name, &input, options.as_ref())
                                .map_err(|error| Exception::throw_message(&ctx, &error))?,
                        )
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let host_exec_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_host_exec_json",
                Func::from(
                    move |ctx: Ctx<'_>, argv_json: String, options_json: String| {
                        let argv = parse_json_string::<Value>(&ctx, &argv_json)?;
                        let options =
                            parse_json_string::<Option<HostCommandOptions>>(&ctx, &options_json)?
                                .unwrap_or_default();
                        to_json_string(
                            &ctx,
                            run_host_exec(&host_exec_state, &argv, &options).map_err(|error| {
                                Exception::throw_message(&ctx, &error.to_string())
                            })?,
                        )
                    },
                ),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let host_shell_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_host_shell_json",
                Func::from(move |ctx: Ctx<'_>, command: String, options_json: String| {
                    let options =
                        parse_json_string::<Option<HostCommandOptions>>(&ctx, &options_json)?
                            .unwrap_or_default();
                    to_json_string(
                        &ctx,
                        run_host_shell(&host_shell_state, &command, &options)
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        Ok(())
    }

    /// Run `f` over the page universe after hydrating what `select` says
    /// the call reads; `select` sees stored fields only.
    fn with_note_index<S, T>(
        state: &JsEvalState,
        ctx: &Ctx<'_>,
        select: impl FnOnce(&JsNoteIndex) -> Result<(IndexNeed, S), DataviewJsError>,
        f: impl FnOnce(&JsNoteIndex, S) -> rquickjs::Result<T>,
    ) -> rquickjs::Result<T> {
        let mut note_index = state
            .note_index
            .lock()
            .map_err(|_| Exception::throw_message(ctx, "DataviewJS note index lock poisoned"))?;
        let (need, selection) = select(&note_index)
            .map_err(|error| Exception::throw_message(ctx, &error.to_string()))?;
        note_index
            .hydrate(state, need)
            .map_err(|error| Exception::throw_message(ctx, &error.to_string()))?;
        f(&note_index, selection)
    }

    /// Load the page universe under the runtime's read authority. Path/tag
    /// grants and policy apply before hydration so `file.inlinks` and linked
    /// pages never include hidden sources. Reloads after mutations must use
    /// this too, or the scope would silently widen mid-script.
    fn load_scoped_note_index(
        paths: &VaultPaths,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> Result<HashMap<String, NoteRecord>, DataviewJsError> {
        match permissions {
            Some(guard) => {
                crate::properties::load_note_index_with_guard_deferring_hydration(paths, guard)
            }
            None => crate::properties::load_note_index_with_filter_deferring_hydration(paths, None),
        }
        .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn reload_note_index(state: &JsEvalState) -> Result<(), DataviewJsError> {
        let note_index = load_scoped_note_index(&state.paths, state.permissions.as_ref())?;
        let mut current = state.note_index.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS note index lock poisoned".to_string())
        })?;
        let generation = current.generation + 1;
        let retired = std::mem::replace(&mut *current, JsNoteIndex::new(note_index, generation));
        state
            .retired_note_indexes
            .lock()
            .map_err(|_| {
                DataviewJsError::Message("DataviewJS note index lock poisoned".to_string())
            })?
            .push(retired);
        Ok(())
    }

    fn resolve_existing_note_path(
        paths: &VaultPaths,
        note: &str,
    ) -> Result<String, DataviewJsError> {
        if let Ok(resolved) = resolve_note_reference(paths, note) {
            Ok(resolved.path)
        } else {
            let normalized = normalize_vault_path(paths.vault_root(), note, None, true)?;
            if paths.vault_root().join(&normalized).is_file() {
                Ok(normalized)
            } else {
                Err(DataviewJsError::Message(format!("note not found: {note}")))
            }
        }
    }

    fn normalize_note_path_for_write(
        paths: &VaultPaths,
        note: &str,
    ) -> Result<String, DataviewJsError> {
        let normalized = normalize_vault_path(paths.vault_root(), note, None, false)?;
        if Path::new(&normalized).extension().is_none() {
            Ok(PathBuf::from(normalized)
                .with_extension("md")
                .to_string_lossy()
                .replace('\\', "/"))
        } else {
            Ok(normalized)
        }
    }

    fn load_note_details(
        state: &JsEvalState,
        note_index: &HashMap<String, NoteRecord>,
        note: &str,
    ) -> Result<Value, DataviewJsError> {
        let paths = &state.paths;
        let path = resolve_existing_note_path(paths, note)?;
        ensure_read_access(state, &path, "vault.note().details()")?;
        let source = secure_read_to_string(paths.vault_root(), Path::new(&path))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let note = note_by_path(note_index, &path)
            .ok_or_else(|| DataviewJsError::Message(format!("note is not indexed: {path}")))?;
        let config = load_vault_config(paths).config;
        let parsed = parse_document(&source, &config);
        let outline = crate::outline_note(&source, &parsed);
        Ok(serde_json::json!({
            "page": page_object(note, Some(paths.vault_root())),
            "file": dataview_file_object(note, Some(paths.vault_root())),
            "frontmatter": note.frontmatter.clone(),
            "content": source,
            "html": render_note_html(paths, &path, &source).html,
            "outline": outline,
            "headings": parsed.headings.into_iter().map(|heading| serde_json::json!({
                "level": heading.level,
                "text": heading.text,
                "byteOffset": heading.byte_offset,
            })).collect::<Vec<_>>(),
            "blocks": parsed.block_refs.into_iter().map(|block| serde_json::json!({
                "id": block.block_id_text,
                "byteOffset": block.block_id_byte_offset,
                "targetStart": block.target_block_byte_start,
                "targetEnd": block.target_block_byte_end,
            })).collect::<Vec<_>>(),
            "dataview_fields": parsed.inline_fields.into_iter().map(|field| serde_json::json!({
                "key": field.key,
                "value": field.value_text,
                "line": field.line_number,
                "kind": format!("{:?}", field.kind).to_ascii_lowercase(),
            })).collect::<Vec<_>>(),
        }))
    }

    fn read_note_selection(
        state: &JsEvalState,
        note: &str,
        options: &crate::NoteReadOptions,
    ) -> Result<crate::NoteReadSelection, DataviewJsError> {
        let paths = &state.paths;
        let path = resolve_existing_note_path(paths, note)?;
        ensure_read_access(state, &path, "vault.note().read()")?;
        let source = secure_read_to_string(paths.vault_root(), Path::new(&path))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let config = load_vault_config(paths).config;
        let parsed = parse_document(&source, &config);
        crate::read_note(&source, &parsed, options)
            .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn load_note_relationships(
        paths: &VaultPaths,
        note_index: &HashMap<String, NoteRecord>,
        note: &str,
        direction: &str,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> Result<Vec<Value>, DataviewJsError> {
        let path = resolve_existing_note_path(paths, note)?;
        let read_filter = permissions.map(PermissionGuard::read_filter);
        match direction {
            "outgoing" => query_links_with_filter(paths, &path, read_filter.as_ref())
                .map_err(|error| DataviewJsError::Message(error.to_string()))?
                .links
                .into_iter()
                .map(|record| {
                    let resolved_target_path = record.resolved_target_path.clone();
                    Ok(serde_json::json!({
                        "rawText": record.raw_text,
                        "linkKind": record.link_kind,
                        "displayText": record.display_text,
                        "targetPathCandidate": record.target_path_candidate,
                        "targetHeading": record.target_heading,
                        "targetBlock": record.target_block,
                        "resolvedTargetPath": resolved_target_path,
                        "resolutionStatus": format!("{:?}", record.resolution_status).to_ascii_lowercase(),
                        "context": record.context,
                        "note": resolved_target_path
                            .as_deref()
                            .and_then(|target| note_by_path(note_index, target))
                            .map_or(Value::Null, |note| page_object(note, Some(paths.vault_root()))),
                    }))
                })
                .collect(),
            "incoming" => query_backlinks_with_filter(paths, &path, read_filter.as_ref())
                .map_err(|error| DataviewJsError::Message(error.to_string()))?
                .backlinks
                .into_iter()
                .map(|record| {
                    let source_path = record.source_path.clone();
                    Ok(serde_json::json!({
                        "sourcePath": source_path,
                        "rawText": record.raw_text,
                        "linkKind": record.link_kind,
                        "displayText": record.display_text,
                        "context": record.context,
                        "note": note_by_path(note_index, &source_path)
                            .map_or(Value::Null, |note| page_object(note, Some(paths.vault_root()))),
                    }))
                })
                .collect(),
            other => Err(DataviewJsError::Message(format!(
                "unsupported note relationship direction: {other}"
            ))),
        }
    }

    fn load_note_neighbors(
        paths: &VaultPaths,
        note_index: &HashMap<String, NoteRecord>,
        note: &str,
        depth: i32,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> Result<Vec<Value>, DataviewJsError> {
        let path = resolve_existing_note_path(paths, note)?;
        let read_filter = permissions.map(PermissionGuard::read_filter);
        let max_depth = usize::try_from(depth.max(1)).unwrap_or(1);
        let mut seen = HashSet::from([path.clone()]);
        let mut queue = VecDeque::from([(path.clone(), 0_usize)]);
        let mut neighbors = Vec::new();

        while let Some((current, current_depth)) = queue.pop_front() {
            if current_depth >= max_depth {
                continue;
            }
            let outgoing = query_links_with_filter(paths, &current, read_filter.as_ref())
                .map_err(|error| DataviewJsError::Message(error.to_string()))?
                .links
                .into_iter()
                .filter_map(|record| record.resolved_target_path)
                .collect::<Vec<_>>();
            let incoming = query_backlinks_with_filter(paths, &current, read_filter.as_ref())
                .map_err(|error| DataviewJsError::Message(error.to_string()))?
                .backlinks
                .into_iter()
                .map(|record| record.source_path)
                .collect::<Vec<_>>();
            for related in outgoing.into_iter().chain(incoming) {
                if !seen.insert(related.clone()) {
                    continue;
                }
                if let Some(related_note) = note_by_path(note_index, &related) {
                    neighbors.push(page_object(related_note, Some(paths.vault_root())));
                    queue.push_back((related.clone(), current_depth + 1));
                }
            }
        }

        neighbors.sort_by(|left, right| {
            compare_json_ordering(
                left.get("file")
                    .and_then(|file| file.get("path"))
                    .unwrap_or(&Value::Null),
                right
                    .get("file")
                    .and_then(|file| file.get("path"))
                    .unwrap_or(&Value::Null),
            )
        });
        Ok(neighbors)
    }

    fn begin_transaction(state: &JsEvalState) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, "vault.transaction()", "filesystem writes")?;
        ensure_fs_access(state, "vault.transaction()")?;
        let mut transaction = state.transaction.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
        })?;
        if transaction.is_some() {
            return Err(DataviewJsError::Message(
                "a JS transaction is already active".to_string(),
            ));
        }
        *transaction = Some(JsTransactionState::default());
        Ok(())
    }

    fn commit_transaction(state: &JsEvalState) -> Result<(), DataviewJsError> {
        let transaction = {
            let mut transaction = state.transaction.lock().map_err(|_| {
                DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
            })?;
            transaction.take()
        };
        let Some(transaction) = transaction else {
            return Ok(());
        };
        let Some(committer) = state.mutation_committer.as_ref() else {
            return scan_and_reload_state(state);
        };

        let changes = transaction_changes(state, &transaction)?;
        restore_transaction_originals(state, &transaction)?;
        let handled = match committer.commit(&changes) {
            Ok(handled) => handled,
            Err(error) => {
                scan_and_reload_state(state)?;
                return Err(DataviewJsError::Message(error));
            }
        };
        if !handled {
            if let Err(error) = apply_transaction_changes(state, &changes) {
                restore_transaction_originals(state, &transaction)?;
                return Err(error);
            }
        }
        scan_and_reload_state(state)
    }

    fn rollback_transaction(state: &JsEvalState) -> Result<(), DataviewJsError> {
        let originals = {
            let mut transaction = state.transaction.lock().map_err(|_| {
                DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
            })?;
            transaction.take()
        };
        let Some(transaction) = originals else {
            return Ok(());
        };

        restore_transaction_originals(state, &transaction)?;
        scan_and_reload_state(state)
    }

    fn transaction_changes(
        state: &JsEvalState,
        transaction: &JsTransactionState,
    ) -> Result<Vec<DataviewJsMutationChange>, DataviewJsError> {
        let mut paths = transaction.originals.keys().cloned().collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let absolute = state.paths.vault_root().join(&path);
                let after = if absolute.is_file() {
                    Some(
                        fs::read_to_string(&absolute)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?,
                    )
                } else {
                    None
                };
                Ok(DataviewJsMutationChange {
                    before: transaction.originals.get(&path).cloned().flatten(),
                    path,
                    after,
                })
            })
            .collect()
    }

    fn restore_transaction_originals(
        state: &JsEvalState,
        transaction: &JsTransactionState,
    ) -> Result<(), DataviewJsError> {
        let mut originals = transaction.originals.iter().collect::<Vec<_>>();
        originals.sort_by_key(|(left, _)| *left);
        for (path, original) in originals {
            let absolute = state.paths.vault_root().join(path);
            match original {
                Some(contents) => {
                    if let Some(parent) = absolute.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                    }
                    write_vault_file(&absolute, contents)?;
                }
                None => {
                    if absolute.is_file() {
                        fs::remove_file(&absolute)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_transaction_changes(
        state: &JsEvalState,
        changes: &[DataviewJsMutationChange],
    ) -> Result<(), DataviewJsError> {
        for change in changes {
            let absolute = state.paths.vault_root().join(&change.path);
            match change.after.as_ref() {
                Some(contents) => {
                    if let Some(parent) = absolute.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                    }
                    write_vault_file(&absolute, contents)?;
                }
                None if absolute.is_file() => fs::remove_file(&absolute)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?,
                None => {}
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    fn apply_js_mutation(
        state: &JsEvalState,
        kind: &str,
        payload: Value,
    ) -> Result<Value, DataviewJsError> {
        let needs_implicit_transaction = state.mutation_committer.is_some()
            && state
                .transaction
                .lock()
                .map_err(|_| {
                    DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
                })?
                .is_none();
        // Refactors cannot join a transaction; each commits its own plan.
        if !needs_implicit_transaction || kind.starts_with("refactor.") {
            return apply_js_mutation_inner(state, kind, payload);
        }

        begin_transaction(state)?;
        match apply_js_mutation_inner(state, kind, payload) {
            Ok(mut value) => {
                commit_transaction(state)?;
                if let Some(path) = value
                    .pointer("/note/file/path")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    let authoritative = mutation_note_response(state, &path)?;
                    value["note"] = authoritative["note"].clone();
                }
                Ok(value)
            }
            Err(error) => {
                rollback_transaction(state)?;
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    fn apply_js_mutation_inner(
        state: &JsEvalState,
        kind: &str,
        payload: Value,
    ) -> Result<Value, DataviewJsError> {
        match kind {
            "set" => {
                let path = payload_string(&payload, "path")?;
                let content = payload_string(&payload, "content")?.to_string();
                let preserve_frontmatter = payload_bool(&payload, "preserveFrontmatter");
                let resolved_path = resolve_existing_note_path(&state.paths, path)?;
                ensure_write_access(state, &resolved_path, "vault.set()")?;
                record_transaction_original(state, &resolved_path)?;
                let existing = fs::read_to_string(state.paths.vault_root().join(&resolved_path))
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                let updated = if preserve_frontmatter {
                    preserve_existing_frontmatter(&existing, &content)
                } else {
                    content
                };
                write_vault_file(state.paths.vault_root().join(&resolved_path), updated)?;
                scan_and_reload_state(state)?;
                mutation_note_response(state, &resolved_path)
            }
            "create" => {
                let path =
                    normalize_note_path_for_write(&state.paths, payload_string(&payload, "path")?)?;
                ensure_write_access(state, &path, "vault.create()")?;
                let absolute = state.paths.vault_root().join(&path);
                if absolute.exists() {
                    return Err(DataviewJsError::Message(format!(
                        "destination note already exists: {path}"
                    )));
                }
                record_transaction_original(state, &path)?;
                if let Some(parent) = absolute.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                }
                let content = payload
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let rendered = render_note_document(payload.get("frontmatter"), content)?;
                write_vault_file(&absolute, rendered)?;
                scan_and_reload_state(state)?;
                mutation_note_response(state, &path)
            }
            "append" => {
                let path =
                    resolve_existing_note_path(&state.paths, payload_string(&payload, "path")?)?;
                ensure_write_access(state, &path, "vault.append()")?;
                let absolute = state.paths.vault_root().join(&path);
                let existing = fs::read_to_string(&absolute)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                record_transaction_original(state, &path)?;
                let text = payload_string(&payload, "text")?;
                let heading = payload.get("heading").and_then(Value::as_str);
                let prepend = payload_bool(&payload, "prepend");
                let updated = if prepend {
                    prepend_entry_after_frontmatter(&existing, text)
                } else if let Some(heading) = heading {
                    append_under_heading(&existing, heading, text)
                } else {
                    append_at_end(&existing, text)
                };
                write_vault_file(&absolute, updated)?;
                scan_and_reload_state(state)?;
                mutation_note_response(state, &path)
            }
            "patch" => {
                let path =
                    resolve_existing_note_path(&state.paths, payload_string(&payload, "path")?)?;
                ensure_write_access(state, &path, "vault.patch()")?;
                let absolute = state.paths.vault_root().join(&path);
                let existing = fs::read_to_string(&absolute)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                record_transaction_original(state, &path)?;
                let find = payload_string(&payload, "find")?;
                let replace = payload_string(&payload, "replace")?;
                let regex = payload_bool(&payload, "regex");
                let replace_all = payload_bool(&payload, "replaceAll");
                let (updated, match_count) =
                    apply_note_patch(&existing, find, replace, regex, replace_all)?;
                write_vault_file(&absolute, updated)?;
                scan_and_reload_state(state)?;
                let mut response = mutation_note_response(state, &path)?;
                if let Value::Object(ref mut object) = response {
                    object.insert(
                        "matchCount".to_string(),
                        Value::from(u64::try_from(match_count).unwrap_or(u64::MAX)),
                    );
                }
                Ok(response)
            }
            "update" => {
                let path =
                    resolve_existing_note_path(&state.paths, payload_string(&payload, "path")?)?;
                ensure_write_access(state, &path, "vault.update()")?;
                record_transaction_original(state, &path)?;
                let key = payload_string(&payload, "key")?;
                let value = render_property_value(payload.get("value").unwrap_or(&Value::Null))?;
                set_note_property(&state.paths, &path, key, Some(value.as_str()), false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                mutation_note_response(state, &path)
            }
            "unset" => {
                let path =
                    resolve_existing_note_path(&state.paths, payload_string(&payload, "path")?)?;
                ensure_write_access(state, &path, "vault.unset()")?;
                record_transaction_original(state, &path)?;
                let key = payload_string(&payload, "key")?;
                set_note_property(&state.paths, &path, key, None, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                mutation_note_response(state, &path)
            }
            "inbox" => {
                let path = normalize_note_path_for_write(&state.paths, &state.inbox_config.path)?;
                ensure_write_access(state, &path, "vault.inbox()")?;
                record_transaction_original(state, &path)?;
                let absolute = state.paths.vault_root().join(&path);
                if let Some(parent) = absolute.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                }
                let existing = read_existing_or_empty(&absolute)?;
                let entry = render_inbox_entry(
                    &state.inbox_config.format,
                    payload_string(&payload, "text")?,
                    &current_local_timestamp_string(),
                    &today_local_string(),
                );
                let rendered_entry = if state.inbox_config.timestamp {
                    format!("{} {}", current_local_timestamp_string(), entry)
                } else {
                    entry
                };
                let updated = state.inbox_config.heading.as_deref().map_or_else(
                    || append_at_end(&existing, &rendered_entry),
                    |heading| append_under_heading(&existing, heading, &rendered_entry),
                );
                write_vault_file(&absolute, updated)?;
                scan_and_reload_state(state)?;
                Ok(serde_json::json!({ "path": path, "appended": true }))
            }
            "daily.append" => {
                let date = normalize_daily_event_date(
                    payload.get("date").and_then(Value::as_str),
                    state.deterministic_static,
                    "vault.daily.append()",
                )?;
                let path =
                    crate::expected_periodic_note_path(&state.periodic_config, "daily", &date)
                        .ok_or_else(|| {
                            DataviewJsError::Message(format!(
                                "failed to resolve daily note path for {date}"
                            ))
                        })?;
                ensure_write_access(state, &path, "vault.daily.append()")?;
                record_transaction_original(state, &path)?;
                let absolute = state.paths.vault_root().join(&path);
                if let Some(parent) = absolute.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                }
                let existing = read_existing_or_empty(&absolute)?;
                let text = payload_string(&payload, "text")?;
                let updated = payload.get("heading").and_then(Value::as_str).map_or_else(
                    || append_at_end(&existing, text),
                    |heading| append_under_heading(&existing, heading, text),
                );
                write_vault_file(&absolute, updated)?;
                scan_and_reload_state(state)?;
                mutation_note_response(state, &path)
            }
            "refactor.renameAlias" => {
                ensure_no_transaction(state, "vault.refactor.renameAlias()")?;
                let note = payload_string(&payload, "note")?;
                let resolved_note = resolve_existing_note_path(&state.paths, note)?;
                ensure_refactor_access(state, &resolved_note, "vault.refactor.renameAlias()")?;
                let old_alias = payload_string(&payload, "oldAlias")?;
                let new_alias = payload_string(&payload, "newAlias")?;
                commit_refactor_plan(
                    state,
                    plan_rename_alias(&state.paths, note, old_alias, new_alias),
                )
            }
            "refactor.renameHeading" => {
                ensure_no_transaction(state, "vault.refactor.renameHeading()")?;
                let note = payload_string(&payload, "note")?;
                let resolved_note = resolve_existing_note_path(&state.paths, note)?;
                ensure_refactor_access(state, &resolved_note, "vault.refactor.renameHeading()")?;
                let old_heading = payload_string(&payload, "oldHeading")?;
                let new_heading = payload_string(&payload, "newHeading")?;
                commit_refactor_plan(
                    state,
                    plan_rename_heading(&state.paths, note, old_heading, new_heading),
                )
            }
            "refactor.renameBlockRef" => {
                ensure_no_transaction(state, "vault.refactor.renameBlockRef()")?;
                let note = payload_string(&payload, "note")?;
                let resolved_note = resolve_existing_note_path(&state.paths, note)?;
                ensure_refactor_access(state, &resolved_note, "vault.refactor.renameBlockRef()")?;
                let old_block_id = payload_string(&payload, "oldBlockId")?;
                let new_block_id = payload_string(&payload, "newBlockId")?;
                commit_refactor_plan(
                    state,
                    plan_rename_block_ref(&state.paths, note, old_block_id, new_block_id),
                )
            }
            "refactor.renameProperty" => {
                ensure_no_transaction(state, "vault.refactor.renameProperty()")?;
                ensure_unrestricted_refactor_scope(state, "vault.refactor.renameProperty()")?;
                let old_key = payload_string(&payload, "oldKey")?;
                let new_key = payload_string(&payload, "newKey")?;
                commit_refactor_plan(state, plan_rename_property(&state.paths, old_key, new_key))
            }
            "refactor.mergeTags" => {
                ensure_no_transaction(state, "vault.refactor.mergeTags()")?;
                ensure_unrestricted_refactor_scope(state, "vault.refactor.mergeTags()")?;
                let from_tag = payload_string(&payload, "fromTag")?;
                let to_tag = payload_string(&payload, "toTag")?;
                commit_refactor_plan(state, plan_merge_tags(&state.paths, from_tag, to_tag))
            }
            "refactor.move" => {
                ensure_no_transaction(state, "vault.refactor.move()")?;
                let source = payload_string(&payload, "source")?;
                let resolved_source = resolve_existing_note_path(&state.paths, source)?;
                ensure_refactor_access(state, &resolved_source, "vault.refactor.move()")?;
                let destination = normalize_note_path_for_write(
                    &state.paths,
                    payload_string(&payload, "destination")?,
                )?;
                ensure_refactor_access(state, &destination, "vault.refactor.move()")?;
                let routed = state
                    .mutation_committer
                    .as_ref()
                    .and_then(|committer| committer.move_note(&resolved_source, &destination));
                let report = match routed {
                    Some(result) => result.map_err(DataviewJsError::Message)?,
                    None => move_note(&state.paths, source, &destination, false)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?,
                };
                scan_and_reload_state(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
            other => Err(DataviewJsError::Message(format!(
                "unsupported JS mutation kind: {other}"
            ))),
        }
    }

    fn static_mode_denied(operation: &str, capability: &str) -> DataviewJsError {
        DataviewJsError::Message(format!(
            "DataviewJS static mode does not allow {capability} via {operation}"
        ))
    }

    fn js_permissions_summary(state: &JsEvalState) -> Value {
        state.permissions.as_ref().map_or_else(
            || {
                serde_json::json!({
                    "profile": null,
                    "unrestricted": true,
                    "grant": null,
                })
            },
            |permissions| {
                serde_json::json!({
                    "profile": permissions.profile_name(),
                    "unrestricted": false,
                    "grant": permissions.grant(),
                })
            },
        )
    }

    fn js_permission_check(state: &JsEvalState, action: &str, path: Option<&str>) -> Value {
        let Some(permissions) = state.permissions.as_ref() else {
            return serde_json::json!({ "allowed": true, "error": null });
        };
        let result = match action {
            "read" => permissions.check_read_path(path.unwrap_or_default()),
            "write" => permissions.check_write_path(path.unwrap_or_default()),
            "refactor" => permissions.check_refactor_path(path.unwrap_or_default()),
            "git" => permissions.check_git(),
            "index" => permissions.check_index(),
            "execute" => permissions.check_execute(),
            "shell" => permissions.check_shell(),
            "config_read" | "config-read" => permissions.check_config_read(),
            "config_write" | "config-write" => permissions.check_config_write(),
            "network" => permissions.check_network(path.unwrap_or_default()),
            other => {
                return serde_json::json!({
                    "allowed": false,
                    "error": format!("unknown permission capability `{other}`"),
                });
            }
        };
        match result {
            Ok(()) => serde_json::json!({ "allowed": true, "error": null }),
            Err(error) => serde_json::json!({ "allowed": false, "error": error.to_string() }),
        }
    }

    fn static_mode_time_denied(operation: &str) -> DataviewJsError {
        DataviewJsError::Message(format!(
            "DataviewJS static mode does not allow wall-clock time via {operation}; pass an explicit date or timestamp instead"
        ))
    }

    fn ensure_static_mode_allows(
        state: &JsEvalState,
        operation: &str,
        capability: &str,
    ) -> Result<(), DataviewJsError> {
        if state.deterministic_static {
            return Err(static_mode_denied(operation, capability));
        }
        // A read-only render may still read the web; it must not change
        // anything.
        if state.read_only && capability != "network access" {
            return Err(DataviewJsError::Message(format!(
                "{capability} via {operation} is not allowed while rendering a note"
            )));
        }
        Ok(())
    }

    fn current_today(state: &JsEvalState, operation: &str) -> Result<String, DataviewJsError> {
        if state.deterministic_static {
            return Err(static_mode_time_denied(operation));
        }
        Ok(today_local_string())
    }

    fn ensure_fs_access(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        if !sandbox_allows_fs(state.sandbox) {
            return Err(DataviewJsError::Message(format!(
                "{operation} requires --sandbox fs or higher"
            )));
        }
        Ok(())
    }

    fn ensure_network_access(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "network access")?;
        if !sandbox_allows_network(state.sandbox) {
            return Err(DataviewJsError::Message(format!(
                "{operation} requires --sandbox net or higher"
            )));
        }
        if state
            .permissions
            .as_ref()
            .is_some_and(|permissions| !permissions.grant().network)
        {
            return Err(DataviewJsError::Message(format!(
                "permission denied: {operation} requires network access under the selected profile"
            )));
        }
        Ok(())
    }

    fn ensure_execute_access(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "host execution")?;
        if state.sandbox != JsRuntimeSandbox::None {
            return Err(DataviewJsError::Message(format!(
                "{operation} requires --sandbox none"
            )));
        }
        let permissions = state.permissions.as_ref().ok_or_else(|| {
            DataviewJsError::Message(format!(
                "permission denied: {operation} requires an explicit profile with execute access"
            ))
        })?;
        permissions
            .check_execute()
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(())
    }

    fn ensure_shell_access(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "shell execution")?;
        if state.sandbox != JsRuntimeSandbox::None {
            return Err(DataviewJsError::Message(format!(
                "{operation} requires --sandbox none"
            )));
        }
        let permissions = state.permissions.as_ref().ok_or_else(|| {
            DataviewJsError::Message(format!(
                "permission denied: {operation} requires an explicit profile with shell and execute access"
            ))
        })?;
        permissions
            .check_shell()
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        permissions
            .check_execute()
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(())
    }

    fn ensure_write_access(
        state: &JsEvalState,
        path: &str,
        operation: &str,
    ) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "filesystem writes")?;
        ensure_fs_access(state, operation)?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_write_path(path)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        Ok(())
    }

    fn ensure_read_access(
        state: &JsEvalState,
        path: &str,
        operation: &str,
    ) -> Result<(), DataviewJsError> {
        ensure_fs_access(state, operation)?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_read_path(path)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        Ok(())
    }

    fn ensure_refactor_access(
        state: &JsEvalState,
        path: &str,
        operation: &str,
    ) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "filesystem writes")?;
        ensure_fs_access(state, operation)?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_refactor_path(path)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        Ok(())
    }

    fn ensure_unrestricted_refactor_scope(
        state: &JsEvalState,
        operation: &str,
    ) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "filesystem writes")?;
        ensure_fs_access(state, operation)?;
        if let Some(permissions) = state.permissions.as_ref() {
            let filter = permissions.refactor_filter();
            if !filter.path_permission().is_unrestricted() {
                return Err(DataviewJsError::Message(format!(
                    "permission denied: {operation} requires unrestricted refactor scope under the selected profile"
                )));
            }
        }
        Ok(())
    }

    /// Apply a planned refactor: through the host's committer when it
    /// manages a touched note (an mdbase record), otherwise as an ordinary
    /// batch that refuses files changed since planning.
    fn commit_refactor_plan(
        state: &JsEvalState,
        plan: Result<crate::refactor::RefactorWritePlan, crate::refactor::RefactorError>,
    ) -> Result<Value, DataviewJsError> {
        let plan = plan.map_err(|error| DataviewJsError::Message(error.to_string()))?;
        if !plan.changes.is_empty() {
            let changes = plan
                .changes
                .iter()
                .map(|change| DataviewJsMutationChange {
                    path: change.path.clone(),
                    before: change.before.clone(),
                    after: change.after.clone(),
                })
                .collect::<Vec<_>>();
            let handled = match state.mutation_committer.as_ref() {
                Some(committer) => committer
                    .commit(&changes)
                    .map_err(DataviewJsError::Message)?,
                None => false,
            };
            if !handled {
                crate::ordinary_write::apply_ordinary_write_batch(&state.paths, &plan.changes)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            }
        }
        scan_and_reload_state(state)?;
        let mut report = plan.report;
        report.dry_run = false;
        serde_json::to_value(report).map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn ensure_no_transaction(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        let transaction = state.transaction.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
        })?;
        if transaction.is_some() {
            Err(DataviewJsError::Message(format!(
                "{operation} is not supported inside vault.transaction()"
            )))
        } else {
            Ok(())
        }
    }

    fn record_transaction_original(state: &JsEvalState, path: &str) -> Result<(), DataviewJsError> {
        let mut transaction = state.transaction.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
        })?;
        let Some(transaction) = transaction.as_mut() else {
            return Ok(());
        };
        if transaction.originals.contains_key(path) {
            return Ok(());
        }
        let absolute = state.paths.vault_root().join(path);
        let original = if absolute.is_file() {
            Some(
                fs::read_to_string(&absolute)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?,
            )
        } else {
            None
        };
        transaction.originals.insert(path.to_string(), original);
        Ok(())
    }

    fn scan_and_reload_state(state: &JsEvalState) -> Result<(), DataviewJsError> {
        scan_vault(&state.paths, ScanMode::Incremental)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        reload_note_index(state)
    }

    fn run_host_exec(
        state: &JsEvalState,
        argv: &Value,
        options: &HostCommandOptions,
    ) -> Result<HostProcessReport, DataviewJsError> {
        let argv = parse_host_argv(argv)?;
        if state.granted_host_commands.contains(&argv) {
            // A device-granted command runs from any sandbox tier, exactly
            // as granted: no environment overrides that could change what
            // the program does.
            ensure_static_mode_allows(state, "host.exec()", "host execution")?;
            if options.env.as_ref().is_some_and(|env| !env.is_empty()) {
                return Err(DataviewJsError::Message(
                    "host.exec() cannot override the environment of a granted host command"
                        .to_string(),
                ));
            }
        } else {
            ensure_execute_access(state, "host.exec()").map_err(|error| {
                if state.granted_host_commands.is_empty() {
                    error
                } else {
                    DataviewJsError::Message(format!(
                        "{error}; this tool's granted host commands are {:?}",
                        state.granted_host_commands
                    ))
                }
            })?;
        }
        let cwd = resolve_host_cwd(state, options.cwd.as_deref())?;
        let timeout = effective_host_timeout(options.timeout_ms, state.runtime_timeout)?;
        let max_output_bytes = effective_host_max_output_bytes(options.max_output_bytes)?;
        let mut process = ProcessCommand::new(&argv[0]);
        process
            .args(&argv[1..])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_host_env_overrides(&mut process, options.env.as_ref());
        run_host_process(
            process,
            HostInvocation::Exec { argv },
            &cwd,
            timeout,
            max_output_bytes,
        )
    }

    fn run_host_shell(
        state: &JsEvalState,
        command: &str,
        options: &HostCommandOptions,
    ) -> Result<HostProcessReport, DataviewJsError> {
        ensure_shell_access(state, "host.shell()")?;
        let command = command.trim();
        if command.is_empty() {
            return Err(DataviewJsError::Message(
                "host.shell() requires a non-empty command".to_string(),
            ));
        }
        let cwd = resolve_host_cwd(state, options.cwd.as_deref())?;
        let timeout = effective_host_timeout(options.timeout_ms, state.runtime_timeout)?;
        let max_output_bytes = effective_host_max_output_bytes(options.max_output_bytes)?;
        let shell = state
            .shell_path
            .as_deref()
            .unwrap_or(default_system_shell());
        let mut process = ProcessCommand::new(shell);
        configure_host_shell_command(&mut process, state.shell_path.as_deref(), command);
        process
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_host_env_overrides(&mut process, options.env.as_ref());
        run_host_process(
            process,
            HostInvocation::Shell {
                command: command.to_string(),
                shell: shell.display().to_string(),
            },
            &cwd,
            timeout,
            max_output_bytes,
        )
    }

    fn parse_host_argv(argv: &Value) -> Result<Vec<String>, DataviewJsError> {
        let Value::Array(items) = argv else {
            return Err(DataviewJsError::Message(
                "host.exec() requires argv to be a JSON array of strings".to_string(),
            ));
        };
        if items.is_empty() {
            return Err(DataviewJsError::Message(
                "host.exec() requires argv[0] to name a program".to_string(),
            ));
        }
        items
            .iter()
            .map(|item| match item {
                Value::String(value) => Ok(value.clone()),
                _ => Err(DataviewJsError::Message(
                    "host.exec() requires argv to be a JSON array of strings".to_string(),
                )),
            })
            .collect()
    }

    fn resolve_host_cwd(
        state: &JsEvalState,
        cwd: Option<&str>,
    ) -> Result<PathBuf, DataviewJsError> {
        let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) else {
            return Ok(state.paths.vault_root().to_path_buf());
        };
        let normalized = normalize_vault_path(
            state.paths.vault_root(),
            cwd,
            state.current_file.as_deref(),
            false,
        )?;
        let absolute = state.paths.vault_root().join(&normalized);
        if !absolute.is_dir() {
            return Err(DataviewJsError::Message(format!(
                "host execution cwd `{normalized}` is not a directory"
            )));
        }
        Ok(absolute)
    }

    fn effective_host_timeout(
        requested_timeout_ms: Option<u64>,
        runtime_timeout: Option<Duration>,
    ) -> Result<Duration, DataviewJsError> {
        let inherited_timeout = runtime_timeout.unwrap_or(DEFAULT_HOST_TIMEOUT);
        match requested_timeout_ms {
            Some(0) => Err(DataviewJsError::Message(
                "host execution timeout must be greater than 0ms".to_string(),
            )),
            Some(timeout_ms) => Ok(inherited_timeout.min(Duration::from_millis(timeout_ms))),
            None => Ok(inherited_timeout),
        }
    }

    fn effective_host_max_output_bytes(
        requested_max_output_bytes: Option<usize>,
    ) -> Result<usize, DataviewJsError> {
        match requested_max_output_bytes {
            Some(0) => Err(DataviewJsError::Message(
                "host execution max_output_bytes must be greater than 0".to_string(),
            )),
            Some(max_output_bytes) => Ok(max_output_bytes.min(MAX_HOST_MAX_OUTPUT_BYTES)),
            None => Ok(DEFAULT_HOST_MAX_OUTPUT_BYTES),
        }
    }

    fn apply_host_env_overrides(process: &mut ProcessCommand, env: Option<&Map<String, Value>>) {
        let Some(env) = env else {
            return;
        };
        for (key, value) in env {
            match value {
                Value::Null => {
                    process.env_remove(key);
                }
                other => {
                    process.env(key, json_value_to_host_env_string(other));
                }
            }
        }
    }

    fn json_value_to_host_env_string(value: &Value) -> String {
        match value {
            Value::Null => String::new(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::String(value) => value.clone(),
            Value::Array(items) => items
                .iter()
                .map(json_value_to_host_env_string)
                .collect::<Vec<_>>()
                .join(","),
            Value::Object(object) => Value::Object(object.clone()).to_string(),
        }
    }

    fn run_host_process(
        mut process: ProcessCommand,
        invocation: HostInvocation,
        cwd: &Path,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<HostProcessReport, DataviewJsError> {
        let mut child = process
            .spawn()
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DataviewJsError::Message("failed to capture stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| DataviewJsError::Message("failed to capture stderr".to_string()))?;
        let stdout_handle =
            thread::spawn(move || read_captured_process_output(stdout, max_output_bytes));
        let stderr_handle =
            thread::spawn(move || read_captured_process_output(stderr, max_output_bytes));
        let started = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| DataviewJsError::Message(error.to_string()))?
            {
                break status;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let stdout = stdout_handle
            .join()
            .map_err(|_| DataviewJsError::Message("stdout capture thread panicked".to_string()))?
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let stderr = stderr_handle
            .join()
            .map_err(|_| DataviewJsError::Message("stderr capture thread panicked".to_string()))?
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        Ok(HostProcessReport {
            invocation,
            cwd: cwd.display().to_string(),
            success: status.success() && !timed_out,
            exit_code: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            truncated_stdout: stdout.truncated,
            truncated_stderr: stderr.truncated,
            timed_out,
            duration_ms: started.elapsed().as_millis(),
        })
    }

    fn read_captured_process_output(
        mut reader: impl Read,
        max_output_bytes: usize,
    ) -> std::io::Result<CapturedProcessOutput> {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 8192];
        let mut truncated = false;
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            if buffer.len() < max_output_bytes {
                let remaining = max_output_bytes - buffer.len();
                let take = remaining.min(read);
                buffer.extend_from_slice(&chunk[..take]);
                if take < read {
                    truncated = true;
                }
            } else {
                truncated = true;
            }
        }
        Ok(CapturedProcessOutput {
            text: String::from_utf8_lossy(&buffer).into_owned(),
            truncated,
        })
    }

    fn default_system_shell() -> &'static Path {
        #[cfg(target_os = "windows")]
        {
            Path::new("powershell")
        }
        #[cfg(not(target_os = "windows"))]
        {
            Path::new("/bin/sh")
        }
    }

    fn configure_host_shell_command(
        process: &mut ProcessCommand,
        shell: Option<&Path>,
        command: &str,
    ) {
        #[cfg(target_os = "windows")]
        {
            let shell_name = shell
                .and_then(|value| value.file_name())
                .and_then(|value| value.to_str())
                .unwrap_or("powershell")
                .to_ascii_lowercase();
            if shell_name == "cmd" || shell_name == "cmd.exe" {
                process.arg("/C").arg(command);
            } else if shell_name.contains("powershell")
                || shell_name == "pwsh"
                || shell_name == "pwsh.exe"
            {
                process.arg("-NoProfile").arg("-Command").arg(command);
            } else {
                process.arg("-lc").arg(command);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = shell;
            process.arg("-lc").arg(command);
        }
    }

    fn mutation_note_response(state: &JsEvalState, path: &str) -> Result<Value, DataviewJsError> {
        let mut note_index = state.note_index.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS note index lock poisoned".to_string())
        })?;
        note_index.hydrate(state, IndexNeed::Paths(vec![path.to_string()]))?;
        let note = note_index
            .note(path)
            .ok_or_else(|| DataviewJsError::Message(format!("note is not indexed: {path}")))?;
        Ok(serde_json::json!({
            "note": page_object(note, Some(state.paths.vault_root())),
        }))
    }

    fn payload_string<'a>(payload: &'a Value, key: &str) -> Result<&'a str, DataviewJsError> {
        payload
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| DataviewJsError::Message(format!("missing string field `{key}`")))
    }

    fn payload_bool(payload: &Value, key: &str) -> bool {
        payload.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    fn render_note_document(
        frontmatter: Option<&Value>,
        body: &str,
    ) -> Result<String, DataviewJsError> {
        let mut rendered = String::new();
        if let Some(frontmatter) = frontmatter.filter(|value| !value.is_null()) {
            if !frontmatter.is_object() {
                return Err(DataviewJsError::Message(
                    "note frontmatter must be a JSON object".to_string(),
                ));
            }
            let yaml_source = yaml_serde::to_string(frontmatter)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            let yaml = strip_yaml_doc_marker(&yaml_source);
            rendered.push_str("---\n");
            rendered.push_str(yaml.trim_end_matches('\n'));
            rendered.push_str("\n---");
            if body.is_empty() {
                rendered.push('\n');
            } else {
                rendered.push_str("\n\n");
            }
        }
        rendered.push_str(body);
        Ok(rendered)
    }

    fn preserve_existing_frontmatter(existing: &str, body: &str) -> String {
        find_frontmatter_block(existing).map_or_else(
            || body.to_string(),
            |(_, _, body_start)| {
                let mut rendered = existing[..body_start].to_string();
                rendered.push_str(body);
                rendered
            },
        )
    }

    fn strip_yaml_doc_marker(yaml: &str) -> &str {
        yaml.strip_prefix("---\n").unwrap_or(yaml)
    }

    fn render_property_value(value: &Value) -> Result<String, DataviewJsError> {
        let yaml = yaml_serde::to_string(value)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(strip_yaml_doc_marker(&yaml).trim().to_string())
    }

    fn find_frontmatter_block(source: &str) -> Option<(usize, usize, usize)> {
        let mut lines = source.split_inclusive('\n');
        let first_line = lines.next()?;
        if !matches!(first_line, "---\n" | "---\r\n" | "---") {
            return None;
        }

        let yaml_start = first_line.len();
        let mut offset = yaml_start;
        for line in lines {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed == "---" {
                return Some((yaml_start, offset, offset + line.len()));
            }
            offset += line.len();
        }
        None
    }

    fn append_at_end(contents: &str, entry: &str) -> String {
        let mut prefix = contents.trim_end_matches('\n').to_string();
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
        prefix.push_str(entry.trim_end());
        prefix.push('\n');
        prefix
    }

    fn prepend_entry_after_frontmatter(contents: &str, entry: &str) -> String {
        let body_start =
            find_frontmatter_block(contents).map_or(0, |(_, _, body_start)| body_start);
        let prefix = &contents[..body_start];
        let body = contents[body_start..].trim_start_matches('\n');
        let mut updated = prefix.to_string();
        updated.push_str(entry.trim_end());
        updated.push('\n');
        if !body.is_empty() {
            updated.push('\n');
            updated.push_str(body.trim_end_matches('\n'));
            updated.push('\n');
        }
        updated
    }

    fn append_under_heading(contents: &str, heading: &str, entry: &str) -> String {
        let heading = heading.trim();
        if heading.is_empty() {
            return append_at_end(contents, entry);
        }

        let heading_level = markdown_heading_level(heading);
        let mut offset = 0usize;
        let mut insert_at = None;
        for line in contents.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if insert_at.is_none() && trimmed == heading {
                insert_at = Some(offset + line.len());
            } else if insert_at.is_some()
                && markdown_heading_level(trimmed).is_some_and(|level| Some(level) <= heading_level)
            {
                insert_at = Some(offset);
                break;
            }
            offset += line.len();
        }

        if let Some(insert_at) = insert_at {
            let mut prefix = String::new();
            prefix.push_str(&contents[..insert_at]);
            if !prefix.ends_with('\n') {
                prefix.push('\n');
            }
            if !prefix.ends_with("\n\n") {
                prefix.push('\n');
            }
            let mut updated = prefix;
            updated.push_str(entry.trim_end());
            updated.push('\n');
            if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
                updated.push('\n');
            }
            updated.push_str(&contents[insert_at..]);
            updated
        } else {
            let mut prefix = contents.trim_end_matches('\n').to_string();
            if !prefix.is_empty() {
                prefix.push_str("\n\n");
            }
            prefix.push_str(heading);
            prefix.push_str("\n\n");
            prefix.push_str(entry.trim_end());
            prefix.push('\n');
            prefix
        }
    }

    fn markdown_heading_level(line: &str) -> Option<usize> {
        let hashes = line.chars().take_while(|ch| *ch == '#').count();
        (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
            .then_some(hashes)
    }

    fn apply_note_patch(
        source: &str,
        find: &str,
        replace: &str,
        regex: bool,
        replace_all: bool,
    ) -> Result<(String, usize), DataviewJsError> {
        let matches = if regex {
            let regex =
                Regex::new(find).map_err(|error| DataviewJsError::Message(error.to_string()))?;
            regex
                .find_iter(source)
                .map(|matched| {
                    if matched.start() == matched.end() {
                        Err(DataviewJsError::Message(
                            "regex patterns for vault.patch() must not match empty strings"
                                .to_string(),
                        ))
                    } else {
                        Ok((
                            matched.start(),
                            matched.end(),
                            regex.replace(matched.as_str(), replace).into_owned(),
                        ))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            source
                .match_indices(find)
                .map(|(start, matched)| (start, start + matched.len(), replace.to_string()))
                .collect::<Vec<_>>()
        };

        match matches.len() {
            0 => Err(DataviewJsError::Message(
                "pattern not found in note".to_string(),
            )),
            count if count > 1 && !replace_all => Err(DataviewJsError::Message(format!(
                "pattern matched {count} times; rerun with opts.all = true to replace every match"
            ))),
            count => {
                let mut updated = source.to_string();
                for (start, end, replacement) in matches.iter().rev() {
                    updated.replace_range(*start..*end, replacement);
                }
                Ok((updated, count))
            }
        }
    }

    fn render_inbox_entry(format: &str, text: &str, datetime: &str, date: &str) -> String {
        format
            .replace("{text}", text.trim_end())
            .replace("{date}", date)
            .replace("{time}", datetime.split('T').nth(1).unwrap_or(datetime))
            .replace("{datetime}", datetime)
    }

    /// Now on the local wall clock with its UTC offset (`Z` in UTC), as the
    /// CLI inbox renders `{datetime}`.
    fn current_local_timestamp_string() -> String {
        let utc = crate::current_utc_timestamp_ms();
        let local = crate::expression::value::DataviewTimeZone::System.localize_utc_ms(utc);
        let offset_minutes = (local - utc) / 60_000;
        let suffix = if offset_minutes == 0 {
            "Z".to_string()
        } else {
            let sign = if offset_minutes < 0 { '-' } else { '+' };
            let minutes = offset_minutes.abs();
            format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
        };
        let seconds = local.div_euclid(1_000);
        let days_since_epoch = seconds.div_euclid(86_400);
        let seconds_of_day = seconds.rem_euclid(86_400);
        let hour = seconds_of_day / 3_600;
        let minute = (seconds_of_day % 3_600) / 60;
        let second = seconds_of_day % 60;
        let (year, month, day) = civil_from_days(days_since_epoch);
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{suffix}")
    }

    fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
        let z = days_since_epoch + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * mp + 2) / 5 + 1;
        let month = mp + if mp < 10 { 3 } else { -9 };
        let year = y + i64::from(month <= 2);
        (year, month, day)
    }

    #[cfg(not(feature = "web"))]
    fn run_js_web_search(
        state: &JsEvalState,
        _query: &str,
        _limit: usize,
    ) -> Result<WebSearchReport, DataviewJsError> {
        ensure_network_access(state, "web.search()")?;
        Err(DataviewJsError::Message(
            "web.search() needs a Vulcan build with the `web` feature".to_string(),
        ))
    }

    #[cfg(not(feature = "web"))]
    fn run_js_web_fetch(
        state: &JsEvalState,
        _url: &str,
        _mode: &str,
    ) -> Result<WebFetchReport, DataviewJsError> {
        ensure_network_access(state, "web.fetch()")?;
        Err(DataviewJsError::Message(
            "web.fetch() needs a Vulcan build with the `web` feature".to_string(),
        ))
    }

    #[cfg(feature = "web")]
    fn run_js_web_search(
        state: &JsEvalState,
        query: &str,
        limit: usize,
    ) -> Result<WebSearchReport, DataviewJsError> {
        ensure_network_access(state, "web.search()")?;
        let prepared =
            prepare_search_backend(&state.web_config, None).map_err(DataviewJsError::Message)?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_network(&prepared.base_url)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        search_web(&state.web_config.user_agent, &prepared, query, limit)
            .map_err(DataviewJsError::Message)
    }

    #[cfg(feature = "web")]
    fn run_js_web_fetch(
        state: &JsEvalState,
        url: &str,
        mode: &str,
    ) -> Result<WebFetchReport, DataviewJsError> {
        ensure_network_access(state, "web.fetch()")?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_network(url)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        fetch_web(&state.web_config, url, mode).map_err(DataviewJsError::Message)
    }

    /// The paths `dv.pages(source)` returns, sorted: every readable note
    /// without a source, else the source's selection within the runtime's
    /// readable universe (QRY.2), so hidden notes neither appear nor resolve.
    fn select_pages(
        state: &JsEvalState,
        note_index: &JsNoteIndex,
        source: Option<&str>,
    ) -> Result<(IndexNeed, Vec<String>), DataviewJsError> {
        let Some(source) = source.filter(|value| !value.trim().is_empty()) else {
            let mut all = note_index.by_path.keys().cloned().collect::<Vec<_>>();
            all.sort();
            return Ok((IndexNeed::All, all));
        };
        let filter = state.permissions.as_ref().map(PermissionGuard::read_filter);
        let mut selected = crate::dql::select_source_paths(
            &state.paths,
            source,
            state.current_file.as_deref(),
            &note_index.notes,
            filter.as_ref(),
        )
        .map_err(|error| DataviewJsError::Message(error.to_string()))?
        .into_iter()
        .collect::<Vec<_>>();
        selected.sort();
        Ok((IndexNeed::Paths(selected.clone()), selected))
    }

    /// Page objects for `paths`; list and task positions need `vault_root`.
    fn pages_for_paths(
        note_index: &JsNoteIndex,
        paths: &[String],
        vault_root: Option<&Path>,
    ) -> Vec<Value> {
        let mut pages = paths
            .iter()
            .filter_map(|path| note_index.note(path))
            .map(|note| page_object(note, vault_root))
            .collect::<Vec<_>>();
        pages.sort_by(|left, right| {
            compare_json_ordering(
                left.get("file")
                    .and_then(|file| file.get("path"))
                    .unwrap_or(&Value::Null),
                right
                    .get("file")
                    .and_then(|file| file.get("path"))
                    .unwrap_or(&Value::Null),
            )
        });
        pages
    }

    /// File-object fields read from hydrated rows (tags, links, tasks,
    /// lists); lazy page objects omit them until first read.
    const LAZY_FILE_FIELDS: [&str; 6] = ["etags", "inlinks", "lists", "outlinks", "tags", "tasks"];

    /// Page objects for `paths` without [`LAZY_FILE_FIELDS`], from stored
    /// fields only.
    fn lazy_pages_for_paths(note_index: &JsNoteIndex, paths: &[String]) -> Vec<Value> {
        let mut pages = pages_for_paths(note_index, paths, None);
        for page in &mut pages {
            if let Some(Value::Object(file)) = page.get_mut("file") {
                for key in LAZY_FILE_FIELDS {
                    file.remove(key);
                }
            }
        }
        pages
    }

    /// One lazy file field of a page from a lazy page set, hydrating the
    /// whole set on first read, in the universe the page came from.
    fn read_lazy_page_field(
        state: &JsEvalState,
        generation: usize,
        set: usize,
        path: &str,
        key: &str,
    ) -> Result<Value, DataviewJsError> {
        if !LAZY_FILE_FIELDS.contains(&key) {
            return Err(DataviewJsError::Message(format!(
                "`{key}` is not a lazy page field"
            )));
        }
        let poisoned =
            || DataviewJsError::Message("DataviewJS note index lock poisoned".to_string());
        let mut current = state.note_index.lock().map_err(|_| poisoned())?;
        let mut retired = state.retired_note_indexes.lock().map_err(|_| poisoned())?;
        let note_index = if current.generation == generation {
            &mut *current
        } else {
            retired
                .iter_mut()
                .find(|index| index.generation == generation)
                .ok_or_else(|| DataviewJsError::Message("unknown page generation".to_string()))?
        };
        if !note_index.hydrated.contains(path) && !note_index.complete {
            let selection = note_index
                .page_sets
                .get(set)
                .cloned()
                .ok_or_else(|| DataviewJsError::Message("unknown page set".to_string()))?;
            note_index.hydrate(state, IndexNeed::Paths(selection))?;
        }
        Ok(note_index.note(path).map_or(Value::Null, |note| {
            dataview_file_field(note, key, Some(state.paths.vault_root()))
        }))
    }

    /// A `file` field as Dataview reports it: tags carry `#` and come in
    /// Dataview's order, body tags before frontmatter tags, and `tags` adds
    /// each tag's parents after it; lists and tasks are shaped as Dataview
    /// serializes them, with positions read from the note under
    /// `vault_root`.
    fn dataview_file_field(note: &NoteRecord, key: &str, vault_root: Option<&Path>) -> Value {
        match key {
            "lists" | "tasks" => {
                let source = vault_root.and_then(|root| {
                    secure_read_to_string(root, Path::new(&note.document_path)).ok()
                });
                let items = crate::file_metadata::dataview_list_objects(note, source.as_deref());
                Value::Array(if key == "tasks" {
                    items
                        .into_iter()
                        .filter(|item| item["task"] == Value::Bool(true))
                        .collect()
                } else {
                    items
                })
            }
            "etags" => Value::Array(
                dataview_explicit_tags(note)
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
            "tags" => {
                let mut tags = Vec::<String>::new();
                for tag in dataview_explicit_tags(note) {
                    let mut current = tag.as_str();
                    loop {
                        if !tags.iter().any(|seen| seen == current) {
                            tags.push(current.to_string());
                        }
                        match current.rfind('/') {
                            Some(slash) => current = &current[..slash],
                            None => break,
                        }
                    }
                }
                Value::Array(tags.into_iter().map(Value::String).collect())
            }
            _ => FileMetadataResolver::field(note, key),
        }
    }

    fn dataview_file_object(note: &NoteRecord, vault_root: Option<&Path>) -> Value {
        let mut object = FileMetadataResolver::object(note);
        if let Value::Object(fields) = &mut object {
            for key in ["tags", "etags", "lists", "tasks"] {
                if fields.contains_key(key) {
                    fields.insert(key.to_string(), dataview_file_field(note, key, vault_root));
                }
            }
        }
        object
    }

    /// The note's tags with `#`, deduplicated: body tags first, then the
    /// frontmatter `tags`/`tag` values, split on commas and spaces as
    /// Dataview splits them.
    fn dataview_explicit_tags(note: &NoteRecord) -> Vec<String> {
        fn collect(value: &Value, into: &mut Vec<String>) {
            match value {
                Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
                Value::Null | Value::Bool(false) => {}
                Value::String(text) => into.extend(
                    text.split(|ch: char| ch == ',' || ch.is_whitespace())
                        .filter(|part| !part.is_empty())
                        .map(str::to_string),
                ),
                other => into.push(other.to_string()),
            }
        }
        let with_hash = |tag: &str| {
            if tag.starts_with('#') {
                tag.to_string()
            } else {
                format!("#{tag}")
            }
        };
        let mut frontmatter = Vec::new();
        if let Value::Object(fields) = &note.frontmatter {
            for (key, value) in fields {
                if key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag") {
                    collect(value, &mut frontmatter);
                }
            }
        }
        let frontmatter = frontmatter
            .iter()
            .map(|tag| with_hash(tag))
            .collect::<Vec<_>>();
        let mut tags = Vec::<String>::new();
        let body = note
            .tags
            .iter()
            .map(|tag| with_hash(tag))
            .filter(|tag| !frontmatter.contains(tag));
        for tag in body.chain(frontmatter.iter().cloned()) {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        tags
    }

    fn resolve_page_path(paths: &VaultPaths, file: &str) -> Result<String, DataviewJsError> {
        resolve_note_reference(paths, file)
            .map(|resolved| resolved.path)
            .or_else(|_| normalize_note_path_for_write(paths, file))
    }

    fn page_object_by_reference(
        paths: &VaultPaths,
        note_index: &std::collections::HashMap<String, NoteRecord>,
        file: &str,
    ) -> Result<Value, DataviewJsError> {
        let resolved_path = resolve_note_reference(paths, file)
            .map(|resolved| resolved.path)
            .or_else(|_| normalize_note_path_for_write(paths, file))?;
        Ok(
            note_by_path(note_index, &resolved_path).map_or(Value::Null, |note| {
                page_object(note, Some(paths.vault_root()))
            }),
        )
    }

    fn note_by_path<'a>(
        note_index: &'a std::collections::HashMap<String, NoteRecord>,
        path: &str,
    ) -> Option<&'a NoteRecord> {
        note_index.values().find(|note| note.document_path == path)
    }

    fn page_object(note: &NoteRecord, vault_root: Option<&Path>) -> Value {
        let mut fields = note
            .properties
            .as_object()
            .cloned()
            .unwrap_or_else(Map::new);
        fields.insert("file".to_string(), dataview_file_object(note, vault_root));
        Value::Object(fields)
    }

    fn load_daily_page_object(
        paths: &VaultPaths,
        periodic_config: &crate::PeriodicConfig,
        note_index: &std::collections::HashMap<String, NoteRecord>,
        date: &str,
        deterministic_static: bool,
        operation: &str,
    ) -> Result<Value, DataviewJsError> {
        let normalized_date =
            normalize_daily_event_date(Some(date), deterministic_static, operation)?;
        let Some(path) = resolve_periodic_note(
            paths.vault_root(),
            periodic_config,
            "daily",
            &normalized_date,
        ) else {
            return Ok(Value::Null);
        };
        let Some(note) = note_by_path(note_index, &path) else {
            return Ok(Value::Null);
        };
        let events = load_events_for_periodic_note(paths, &path)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(decorate_daily_page_object(
            note,
            &normalized_date,
            events,
            paths.vault_root(),
        ))
    }

    fn load_daily_range_objects(
        paths: &VaultPaths,
        note_index: &std::collections::HashMap<String, NoteRecord>,
        from: &str,
        to: &str,
        deterministic_static: bool,
        operation: &str,
    ) -> Result<Vec<Value>, DataviewJsError> {
        let (from, to) =
            normalize_daily_event_range(Some(from), Some(to), deterministic_static, operation)?;
        let daily_notes = list_daily_note_events(paths, &from, &to)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(daily_notes
            .into_iter()
            .filter_map(|note| {
                note_by_path(note_index, &note.path).map(|record| {
                    decorate_daily_page_object(record, &note.date, note.events, paths.vault_root())
                })
            })
            .collect())
    }

    fn decorate_daily_page_object(
        note: &NoteRecord,
        date: &str,
        events: Vec<crate::PeriodicEvent>,
        vault_root: &Path,
    ) -> Value {
        let mut page = page_object(note, Some(vault_root));
        if let Value::Object(ref mut fields) = page {
            fields.insert(
                "events".to_string(),
                serde_json::to_value(events).unwrap_or(Value::Null),
            );
            fields.insert(
                "periodic".to_string(),
                serde_json::json!({
                    "type": "daily",
                    "date": date,
                    "path": note.document_path,
                }),
            );
        }
        page
    }

    fn normalize_daily_event_range(
        from: Option<&str>,
        to: Option<&str>,
        deterministic_static: bool,
        operation: &str,
    ) -> Result<(String, String), DataviewJsError> {
        let today = if deterministic_static {
            None
        } else {
            Some(today_local_string())
        };
        let from_date = match from {
            Some(value) => {
                normalize_daily_event_date(Some(value), deterministic_static, operation)?
            }
            None if to.is_some() => {
                normalize_daily_event_date(to, deterministic_static, operation)?
            }
            None => today
                .clone()
                .ok_or_else(|| static_mode_time_denied(operation))?,
        };
        let to_date = match to {
            Some(value) => {
                normalize_daily_event_date(Some(value), deterministic_static, operation)?
            }
            None if from.is_some() => from_date.clone(),
            None => today.ok_or_else(|| static_mode_time_denied(operation))?,
        };
        if from_date > to_date {
            return Err(DataviewJsError::Message(format!(
                "start date must be before or equal to end date: {from_date} > {to_date}"
            )));
        }
        Ok((from_date, to_date))
    }

    fn normalize_daily_event_date(
        raw: Option<&str>,
        deterministic_static: bool,
        operation: &str,
    ) -> Result<String, DataviewJsError> {
        let value = raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("today");
        if value.eq_ignore_ascii_case("today") {
            return if deterministic_static {
                Err(static_mode_time_denied(operation))
            } else {
                Ok(today_local_string())
            };
        }
        let bytes = value.as_bytes();
        let valid_shape = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
        if valid_shape && parse_date_like_string(value).is_some() {
            Ok(value.to_string())
        } else {
            Err(DataviewJsError::Message(format!(
                "invalid daily note date: {value}"
            )))
        }
    }

    fn read_text_file(
        state: &JsEvalState,
        path: &str,
        origin_file: Option<&str>,
        operation: &str,
    ) -> Result<String, DataviewJsError> {
        let normalized = normalize_vault_path(state.paths.vault_root(), path, origin_file, true)?;
        ensure_read_access(state, &normalized, operation)?;
        secure_read_to_string(state.paths.vault_root(), Path::new(&normalized))
            .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn read_csv_file(
        state: &JsEvalState,
        path: &str,
        origin_file: Option<&str>,
    ) -> Result<Vec<Map<String, Value>>, DataviewJsError> {
        let normalized = normalize_vault_path(state.paths.vault_root(), path, origin_file, false)?;
        ensure_read_access(state, &normalized, "dv.io.csv()")?;
        let contents = secure_read_to_string(state.paths.vault_root(), Path::new(&normalized))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let mut reader = ReaderBuilder::new()
            .flexible(true)
            .from_reader(contents.as_bytes());
        let headers = reader
            .headers()
            .map_err(|error| DataviewJsError::Message(error.to_string()))?
            .iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|error| DataviewJsError::Message(error.to_string()))?;
            let mut row = Map::new();
            for (index, value) in record.iter().enumerate() {
                let key = headers
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("column_{index}"));
                row.insert(key, Value::String(value.to_string()));
            }
            rows.push(row);
        }
        Ok(rows)
    }

    fn normalize_vault_path(
        vault_root: &Path,
        path: &str,
        origin_file: Option<&str>,
        append_markdown_extension: bool,
    ) -> Result<String, DataviewJsError> {
        let relative =
            if let Some(origin_file) = origin_file.filter(|_| !Path::new(path).is_absolute()) {
                Path::new(origin_file)
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join(path)
            } else {
                PathBuf::from(path)
            };

        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => normalized.push(part),
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(DataviewJsError::Message(
                            "DataviewJS paths must stay inside the vault root".to_string(),
                        ));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(DataviewJsError::Message(
                        "DataviewJS paths must be vault-relative".to_string(),
                    ));
                }
            }
        }

        if append_markdown_extension && normalized.extension().is_none() {
            let markdown_path = normalized.with_extension("md");
            if vault_root.join(&markdown_path).exists() {
                normalized = markdown_path;
            }
        }

        let normalized = normalized.to_string_lossy().replace('\\', "/");
        if normalized.is_empty() {
            return Err(DataviewJsError::Message(
                "DataviewJS paths must resolve to a file inside the vault".to_string(),
            ));
        }
        if !vault_root.join(&normalized).starts_with(vault_root) {
            return Err(DataviewJsError::Message(
                "DataviewJS paths must stay inside the vault root".to_string(),
            ));
        }
        Ok(normalized)
    }

    fn resolved_current_file(
        paths: &VaultPaths,
        requested_file: Option<&str>,
        current_file: Option<&str>,
    ) -> Result<Option<String>, DataviewJsError> {
        match requested_file {
            Some(file) => {
                let resolved = resolve_note_reference(paths, file)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                Ok(Some(resolved.path))
            }
            None => Ok(current_file.map(ToOwned::to_owned)),
        }
    }

    fn evaluate_value<'js>(
        ctx: &Ctx<'js>,
        source: &str,
        strict: bool,
        timeout_description: &str,
    ) -> Result<EvaluatedValue, DataviewJsError> {
        // `EvalOptions` is non-exhaustive, so it can only be built by
        // changing a default.
        #[allow(clippy::field_reassign_with_default)]
        let top_level_await = uses_await(source);
        let options = {
            let mut options = EvalOptions::default();
            options.strict = strict;
            options.promise = top_level_await;
            options
        };
        let value: JsValue<'js> = ctx
            .eval_with_options(source, options)
            .catch(ctx)
            .map_err(|error| map_caught_runtime_error(&error, timeout_description))?;
        // A promise completion value (always the case with top-level
        // `await`, which wraps the value as `{ value }`) settles once pending
        // jobs have run; `take_awaited_value` reads it then.
        if value.is_promise() {
            let await_fn: rquickjs::Function<'js> = ctx
                .globals()
                .get("__vulcanAwaitResult")
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            await_fn
                .call::<_, ()>((value, top_level_await))
                .catch(ctx)
                .map_err(|error| map_caught_runtime_error(&error, timeout_description))?;
            return Ok(EvaluatedValue::Awaiting);
        }
        if value.is_undefined() {
            Ok(EvaluatedValue::Ready(None))
        } else {
            serialize_value(ctx, value, timeout_description)
                .map(|value| EvaluatedValue::Ready(Some(value)))
        }
    }

    enum EvaluatedValue {
        Ready(Option<Value>),
        /// A promise handed to `__vulcanAwaitResult`.
        Awaiting,
    }

    /// The settled value of the promise `evaluate_value` left awaiting.
    fn take_awaited_value(
        ctx: &Ctx<'_>,
        timeout_description: &str,
    ) -> Result<Option<Value>, DataviewJsError> {
        let value: JsValue<'_> = ctx
            .eval("__vulcanTakeResult()")
            .catch(ctx)
            .map_err(|error| map_caught_runtime_error(&error, timeout_description))?;
        if value.is_undefined() {
            Ok(None)
        } else {
            serialize_value(ctx, value, timeout_description).map(Some)
        }
    }

    fn serialize_value<'js>(
        ctx: &Ctx<'js>,
        value: JsValue<'js>,
        timeout_description: &str,
    ) -> Result<Value, DataviewJsError> {
        let serialize_fn: rquickjs::Function<'js> = ctx
            .globals()
            .get("__vulcanSerialize")
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let serialized_json: String = serialize_fn
            .call((value,))
            .catch(ctx)
            .map_err(|error| map_caught_runtime_error(&error, timeout_description))?;
        serde_json::from_str(&serialized_json)
            .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn format_timeout(timeout: Duration) -> String {
        if timeout.as_secs() > 0 && timeout.subsec_nanos() == 0 {
            match timeout.as_secs() {
                1 => "1 second".to_string(),
                seconds => format!("{seconds} seconds"),
            }
        } else {
            format!("{} ms", timeout.as_millis().max(1))
        }
    }

    fn sandbox_uses_resource_limits(sandbox: JsRuntimeSandbox) -> bool {
        sandbox != JsRuntimeSandbox::None
    }

    fn sandbox_allows_fs(sandbox: JsRuntimeSandbox) -> bool {
        matches!(
            sandbox,
            JsRuntimeSandbox::Fs | JsRuntimeSandbox::Net | JsRuntimeSandbox::None
        )
    }

    fn sandbox_allows_network(sandbox: JsRuntimeSandbox) -> bool {
        matches!(sandbox, JsRuntimeSandbox::Net | JsRuntimeSandbox::None)
    }

    fn runtime_memory_limit_bytes(
        config: &JsRuntimeConfig,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> usize {
        let configured = config.memory_limit_mb;
        permissions
            .and_then(|permissions| permissions.resource_limits().memory_limit_mb)
            .map_or(configured, |limit| configured.min(limit))
            .saturating_mul(1024 * 1024)
    }

    fn runtime_stack_limit_bytes(
        config: &JsRuntimeConfig,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> usize {
        let configured = config.stack_limit_kb;
        permissions
            .and_then(|permissions| permissions.resource_limits().stack_limit_kb)
            .map_or(configured, |limit| configured.min(limit))
            .saturating_mul(1024)
    }

    fn effective_timeout(
        config: &VaultConfig,
        sandbox: JsRuntimeSandbox,
        requested: Option<Duration>,
        permissions: Option<&ProfilePermissionGuard>,
    ) -> Result<Option<Duration>, DataviewJsError> {
        let timeout = if let Some(timeout) = requested {
            if timeout.is_zero() {
                return Err(DataviewJsError::Message(
                    "DataviewJS timeout must be greater than 0ms".to_string(),
                ));
            }
            Some(timeout)
        } else if sandbox == JsRuntimeSandbox::None {
            None
        } else {
            Some(Duration::from_secs(
                u64::try_from(config.js_runtime.default_timeout_seconds).unwrap_or(u64::MAX),
            ))
        };
        let Some(timeout) = timeout else {
            return Ok(None);
        };
        let timeout = permissions
            .and_then(|permissions| permissions.resource_limits().cpu_limit_ms)
            .map_or(timeout, |limit_ms| {
                timeout.min(Duration::from_millis(
                    u64::try_from(limit_ms).unwrap_or(u64::MAX),
                ))
            });
        Ok(Some(timeout))
    }

    fn map_runtime_message(message: &str, timeout_description: &str) -> DataviewJsError {
        if message.to_ascii_lowercase().contains("interrupted") {
            DataviewJsError::Message(format!(
                "DataviewJS execution timed out after {timeout_description}"
            ))
        } else {
            DataviewJsError::Message(message.trim().to_string())
        }
    }

    fn map_caught_runtime_error(
        error: &CaughtError<'_>,
        timeout_description: &str,
    ) -> DataviewJsError {
        map_runtime_message(&error.to_string(), timeout_description)
    }

    fn drain_pending_jobs(
        runtime: &Runtime,
        timeout_description: &str,
    ) -> Result<(), DataviewJsError> {
        while runtime.is_job_pending() {
            runtime.execute_pending_job().map_err(|error| {
                error.0.with(|ctx| {
                    map_caught_runtime_error(
                        &CaughtError::from_error(&ctx, rquickjs::Error::Exception),
                        timeout_description,
                    )
                })
            })?;
        }
        Ok(())
    }

    fn compare_json_values(left: &Value, right: &Value) -> i32 {
        let ordering = compare_json_ordering(left, right);
        match ordering {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }

    fn compare_json_ordering(left: &Value, right: &Value) -> Ordering {
        compare_values(left, right)
            .unwrap_or_else(|| value_to_display(left).cmp(&value_to_display(right)))
    }

    fn group_keys_equal(left: &Value, right: &Value) -> bool {
        compare_values(left, right) == Some(Ordering::Equal) || left == right
    }

    fn to_json_string<T>(ctx: &Ctx<'_>, value: T) -> rquickjs::Result<String>
    where
        T: Serialize,
    {
        serde_json::to_string(&value)
            .map_err(|error| Exception::throw_message(ctx, &error.to_string()))
    }

    fn parse_json_string<T>(ctx: &Ctx<'_>, json: &str) -> rquickjs::Result<T>
    where
        T: DeserializeOwned,
    {
        serde_json::from_str(json)
            .map_err(|error| Exception::throw_message(ctx, &error.to_string()))
    }

    /// Writes a vault file atomically, so an interrupted script never leaves
    /// a truncated note behind.
    fn write_vault_file(
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), DataviewJsError> {
        crate::paths::write_file_atomic(path.as_ref(), contents)
            .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    /// A note's contents for appending, empty only when it does not exist
    /// yet; an unreadable note is an error rather than something to overwrite.
    fn read_existing_or_empty(path: &Path) -> Result<String, DataviewJsError> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(DataviewJsError::Message(format!(
                "cannot read {}: {error}",
                path.display()
            ))),
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::path::Path;
        use std::thread;
        use std::time::{Duration, Instant};

        use tempfile::tempdir;

        use crate::{scan_vault, ScanMode};

        use super::*;

        struct RecordingMutationCommitter {
            calls: Mutex<Vec<Vec<DataviewJsMutationChange>>>,
            reject: bool,
        }

        impl DataviewJsMutationCommitter for RecordingMutationCommitter {
            fn commit(&self, changes: &[DataviewJsMutationChange]) -> Result<bool, String> {
                self.calls
                    .lock()
                    .map_err(|_| "recording committer lock poisoned".to_string())?
                    .push(changes.to_vec());
                if self.reject {
                    Err("proposed script write rejected".to_string())
                } else {
                    Ok(false)
                }
            }
        }

        #[test]
        fn dataviewjs_exposes_current_page_lookup_and_pages_helpers() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r##"
                dv.table(
                  ["Current", "Alpha", "Pages"],
                  [[
                    dv.current().status,
                    dv.page("Projects/Alpha").status,
                    dv.pages("#project").file.name.sort().array().join(", ")
                  ]]
                )
                "##,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.outputs.len(), 1);
            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(
                        headers,
                        &vec![
                            "Current".to_string(),
                            "Alpha".to_string(),
                            "Pages".to_string(),
                        ]
                    );
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0][0], Value::String("draft".to_string()));
                    assert_eq!(rows[0][1], Value::String("active".to_string()));
                    assert_eq!(
                        rows[0][2],
                        Value::String("Alpha, Beta, Dashboard".to_string())
                    );
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_supports_render_helpers_and_execute() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                dv.list(dv.pages('"Projects"').file.name.sort().array());
                dv.execute('TABLE status FROM "Projects" SORT file.name ASC');
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.outputs.len(), 2);
            assert!(
                matches!(&result.outputs[0], DataviewJsOutput::List { items } if items == &vec![
                    Value::String("Alpha".to_string()),
                    Value::String("Beta".to_string()),
                ])
            );
            assert!(
                matches!(&result.outputs[1], DataviewJsOutput::Query { result } if result.query_type == crate::dql::DqlQueryType::Table && result.result_count == 2)
            );
        }

        #[test]
        fn lazy_page_hydration_equals_eager_hydration() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan")).unwrap();
            for (path, contents) in [
                (
                    "A/One.md",
                    "---\ntags: [t, visible]\nstatus: open\nup: '[[Two]]'\n---\n[[Two]]\n- [ ] task one\n- item\n",
                ),
                ("A/Three.md", "---\ntags: [visible]\n---\n[[Deux]]\n- [x] done\n"),
                (
                    "B/Two.md",
                    "---\ntags: [t, visible]\naliases: [Deux]\nup: '[[One]]'\n---\n[[One]]\n",
                ),
                ("Hidden.md", "---\ntags: [t]\n---\n[[One]] [[Two]]\n"),
            ] {
                let target = vault_root.join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, contents).unwrap();
            }
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let mut scoped = crate::permissions::resolve_permission_profile(&paths, None).unwrap();
            scoped.grant.read = crate::permissions::PathPermission {
                allow: vec![crate::permissions::ResourceSpecifier::Tag("visible".into())],
                deny: Vec::new(),
            };
            let describe =
                "(p) => p ? [p.file.path, p.file.tags, p.file.inlinks, p.file.outlinks, \
                p.file.tasks.length, p.file.lists.length, p.up] : null";
            for snippet in [
                "dv.pages('\"A\"').map(describe).array()",
                "dv.pages('#t').map(describe).array()",
                "dv.pages('[[One]]').map(describe).array()",
                "dv.pages('#t and -\"A\"').map(describe).array()",
                "describe(dv.page('Deux'))",
                "describe(dv.current())",
                "dv.pages('\"A\"').map((p) => p.up ? describe(dv.page(String(p.up))) : null).array()",
                "dv.pages().map(describe).array()",
            ] {
                for permissions in [None, Some(scoped.clone())] {
                    let run = |prefix: &str| {
                        evaluate_dataview_js_with_options(
                            &paths,
                            &format!("const describe = {describe}; {prefix} dv.paragraph(JSON.stringify({snippet}));"),
                            Some("A/One.md"),
                            DataviewJsEvalOptions {
                                resolved_permissions: permissions.clone(),
                                ..DataviewJsEvalOptions::default()
                            },
                        )
                        .unwrap_or_else(|error| panic!("{snippet}: {error}"))
                        .outputs
                    };
                    assert_eq!(
                        run(""),
                        run("dv.pages();"),
                        "{snippet} scoped={}",
                        permissions.is_some()
                    );
                }
            }
        }

        #[test]
        fn lazy_page_fields_serialize_like_eager_pages() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan")).unwrap();
            for (path, contents) in [
                (
                    "A/One.md",
                    "---\ntags: [t]\nstatus: open\ndue: 2026-01-02\n---\n#inline [[Two]]\n- [ ] task one\n- item\n",
                ),
                ("A/2026-01-01.md", "---\ntags: [t/sub]\n---\n[[One]]\n- [x] done\n"),
                ("B/Two.md", "---\naliases: [Deux]\n---\n[[One]]\n"),
            ] {
                let target = vault_root.join(path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, contents).unwrap();
            }
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let check = |script: &str| {
                let result = evaluate_dataview_js(&paths, script, Some("A/One.md"))
                    .unwrap_or_else(|error| panic!("{script}: {error}"));
                assert_eq!(
                    result.outputs,
                    vec![DataviewJsOutput::Paragraph {
                        text: "ok".to_string()
                    }],
                    "{script}"
                );
            };
            for source in ["'\"A\"'", "'#t'", "'[[One]]'", "null", "'\"B\" or #t'"] {
                check(&format!(
                    "const eager = JSON.parse(__vulcan_pages_json({source})).map(__vulcanRevivePage);
                     const lazy = dv.pages({source}).array();
                     const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
                     if (!same(lazy, eager)) throw new Error(JSON.stringify([lazy, eager]));
                     const keys = (pages) => pages.map((p) => Object.keys(p.file).join());
                     if (!same(keys(dv.pages({source}).array()), keys(eager))) throw new Error('order');
                     if (!same(dv.pages({source}).array().map((p) => ({{...p.file}})), eager.map((p) => p.file)))
                       throw new Error('spread');
                     dv.paragraph('ok');"
                ));
            }
            check(
                "const page = dv.pages('\"A\"').array()[0];
                 page.file.tags = ['x'];
                 if (page.file.tags.join() !== 'x') throw new Error('assign');
                 const other = dv.pages('\"A\"').array()[0];
                 if (other.file.tags.join() === 'x') throw new Error('shared');
                 if (other.file.day.toFormat('yyyy-MM-dd') !== '2026-01-01') throw new Error('day');
                 dv.paragraph('ok');",
            );
        }

        #[test]
        fn dataviewjs_supports_csv_loading_and_external_views() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r#"
                (async () => {
                  const rows = await dv.io.csv("data/owners.csv");
                  dv.paragraph(rows.where((row) => row.role === "editor").name.array().join(", "));
                  await dv.view("views/status-table.js", { title: "Statuses" });
                })();
                "#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.outputs.len(), 3);
            assert_eq!(
                result.outputs[0],
                DataviewJsOutput::Paragraph {
                    text: "Bob".to_string(),
                }
            );
            assert_eq!(
                result.outputs[1],
                DataviewJsOutput::Header {
                    level: 2,
                    text: "Statuses".to_string(),
                }
            );
            assert!(
                matches!(&result.outputs[2], DataviewJsOutput::Table { headers, rows } if headers == &vec!["Name".to_string(), "Status".to_string()] && rows.len() == 3)
            );
        }

        #[test]
        fn dataviewjs_exposes_dataview_helper_members() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const failed = dv.evaluate("1 +");
                let thrown = null;
                try { dv.tryEvaluate("1 +"); } catch (error) { thrown = error.message; }
                ({
                  path: dv.currentFilePath,
                  pagePaths: dv.pagePaths('"Projects"').sort().array(),
                  sum: dv.tryEvaluate("a + b", { a: 2, b: 3 }),
                  thisStatus: dv.tryEvaluate("this.status"),
                  failed: failed.successful,
                  thrownMatches: thrown === failed.error,
                  table: dv.markdownTable(["Name", "N"], [["Alpha", 1], ["B|eta", [1, 2]]]),
                  list: dv.markdownList([1, [2, 3], { a: null }]),
                  tasks: dv.markdownTaskList([
                    { key: "Group", rows: [
                      { path: "a.md", line: 1, task: true, status: "x", text: "Parent",
                        children: [{ path: "a.md", line: 2, parent: 1, task: true, status: " ", text: "Child", children: [] }] },
                      { path: "a.md", line: 2, parent: 1, task: true, status: " ", text: "Child", children: [] },
                    ] },
                  ]),
                  queryIsPromise: dv.query("LIST") instanceof Promise,
                  loadIsPromise: dv.io.load("Dashboard.md").catch(() => null) instanceof Promise,
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.value,
                Some(serde_json::json!({
                    "path": "Dashboard.md",
                    "pagePaths": ["Projects/Alpha.md", "Projects/Beta.md"],
                    "sum": 5,
                    "thisStatus": "draft",
                    "failed": false,
                    "thrownMatches": true,
                    "table": "| Name   | N                             |\n\
                              | ------ | ----------------------------- |\n\
                              | Alpha  | 1                             |\n\
                              | B\\|eta | <ul><li>1</li><li>2</li></ul> |\n",
                    // Dataview passes the same depth to nested values, so
                    // they render as a run-on item, and indents group rows.
                    "list": "- 1\n- - 2\n- 3\n\n- - a: \\-\n\n",
                    "tasks": "# Group\n\n    - [x] Parent\n        - [ ] Child\n",
                    "queryIsPromise": true,
                    "loadIsPromise": true,
                }))
            );
        }

        #[test]
        fn dataviewjs_pages_expose_canonical_field_names() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            fs::write(
                vault_root.join("Keys.md"),
                concat!(
                    "---\nProject Status: active\nkept-name: frontmatter\n---\n",
                    "kept name:: inline\n",
                    "Twice Over:: 1\n",
                    "twice  over:: 2\n",
                    "- Item [Item Kind:: list]\n",
                ),
            )
            .expect("note should be written");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const page = dv.page("Keys");
                const dashboard = dv.page("Dashboard");
                ({
                  status: page["project-status"],
                  kept: page["kept-name"],
                  twice: page["twice-over"],
                  itemKind: page["item-kind"],
                  dueDate: dashboard["due-date"].month,
                  emoji: dashboard["🎅"],
                  accented: dashboard["noël"],
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(value["status"], "active");
            // An existing field keeps its name and value.
            assert_eq!(value["kept"], "frontmatter");
            // Pooled in field-name order: Vulcan's property map is sorted by key.
            assert_eq!(value["twice"], serde_json::json!([2, 1]));
            assert_eq!(value["itemKind"], "list");
            assert_eq!(value["dueDate"], 4);
            assert_eq!(value["emoji"], "gifts");
            assert_eq!(value["accented"], "un jeu de console");
        }

        #[test]
        fn dataviewjs_query_results_follow_dataview() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const sorted = 'FROM "Projects" SORT file.name ASC';
                const ids = await dv.tryQuery(`LIST ${sorted}`);
                const pairs = await dv.tryQuery(`LIST status ${sorted}`);
                const bare = await dv.tryQuery(`LIST WITHOUT ID priority ${sorted}`);
                const table = await dv.tryQuery(`TABLE status, priority ${sorted}`);
                const tasks = await dv.tryQuery('TASK FROM "Projects"');
                const grouped = await dv.tryQuery('LIST rows.file.name FROM "Projects" GROUP BY status');
                ({
                  ids: [ids.type, ids.primaryMeaning.type, ids.values.map((link) => link.path)],
                  pairs: pairs.values.map((pair) => [pair.$widget, pair.key.path, pair.value, String(pair)]),
                  bare: [bare.values, Array.isArray(bare.values)],
                  table: [table.type, table.headers, table.idMeaning.type, table.values[1].slice(1)],
                  task: [tasks.type, tasks.values[0].text, tasks.values[0].line, tasks.values[0].due.toISODate(), "reviewed" in tasks.values[0]],
                  grouped: [grouped.primaryMeaning.type, grouped.values.length],
                  markdown: await dv.tryQueryMarkdown(`TABLE status ${sorted}`),
                  pairMarkdown: await dv.tryQueryMarkdown(`LIST status ${sorted}`),
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(
                value["ids"],
                serde_json::json!(["list", "path", ["Projects/Alpha.md", "Projects/Beta.md"]])
            );
            assert_eq!(
                value["pairs"],
                serde_json::json!([
                    [
                        "dataview:list-pair",
                        "Projects/Alpha.md",
                        "active",
                        "[[Projects/Alpha.md|Alpha]]: active"
                    ],
                    [
                        "dataview:list-pair",
                        "Projects/Beta.md",
                        "backlog",
                        "[[Projects/Beta.md|Beta]]: backlog"
                    ],
                ])
            );
            assert_eq!(value["bare"], serde_json::json!([[1, 5], true]));
            assert_eq!(
                value["table"],
                serde_json::json!([
                    "table",
                    ["File", "status", "priority"],
                    "path",
                    ["backlog", 5]
                ])
            );
            assert_eq!(
                value["task"],
                serde_json::json!([
                    "task",
                    "Follow up [due:: 2026-04-02]",
                    8,
                    "2026-04-02",
                    false
                ])
            );
            assert_eq!(value["grouped"], serde_json::json!(["group", 2]));
            assert_eq!(
                value["markdown"],
                "| File                         | status  |\n| ---------------------------- | ------- |\n| [[Projects/Alpha.md\\|Alpha]] | active  |\n| [[Projects/Beta.md\\|Beta]]   | backlog |\n"
            );
            assert_eq!(
                value["pairMarkdown"],
                "- [[Projects/Alpha.md|Alpha]]: active\n- [[Projects/Beta.md|Beta]]: backlog\n"
            );
        }

        #[test]
        fn dataviewjs_task_lists_group_by_file_by_default() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                "const tasks = dv.current().file.tasks;\n\
                 dv.taskList(tasks);\n\
                 dv.taskList(tasks, false);",
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let lists = result
                .outputs
                .iter()
                .map(|output| match output {
                    DataviewJsOutput::TaskList {
                        tasks,
                        group_by_file,
                    } => (
                        *group_by_file,
                        tasks
                            .iter()
                            .map(|task| task["text"].clone())
                            .collect::<Vec<_>>(),
                    ),
                    other => panic!("unexpected output: {other:?}"),
                })
                .collect::<Vec<_>>();
            // `Ship release` sits under `Write docs`, so it shows only there.
            let roots = vec![serde_json::json!("Write docs [due:: 2026-04-01]")];
            assert_eq!(lists, vec![(true, roots.clone()), (false, roots)]);
        }

        #[test]
        fn dataviewjs_list_items_follow_dataview() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const eager = dv.page("Dashboard").file;
                const lazy = dv.pages().where((page) => page.file.name === "Dashboard")[0].file;
                const [item, nested] = eager.lists;
                const [write, ship] = eager.tasks;
                ({
                  item: [item.line, item.list, item.symbol, "parent" in item, item.kind],
                  nested: [nested.line, nested.parent, nested.symbol, nested.blockId, nested.link.subpath],
                  aliases: [item.subtasks === item.children, item.real, item.header.subpath, item.header.type],
                  position: item.position,
                  lazySame: JSON.stringify(lazy.lists.array()) === JSON.stringify(eager.lists.array()),
                  write: [write.status, write.due.toISODate(), write.fullyCompleted, "status" in item],
                  ship: [ship.parent, ship.owner.path, ship.completed, "reviewed" in ship],
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(
                value["item"],
                serde_json::json!([18, 18, "-", false, "note"])
            );
            assert_eq!(
                value["nested"],
                serde_json::json!([19, 18, "1.", "list-child", "list-child"])
            );
            assert_eq!(
                value["aliases"],
                serde_json::json!([true, false, "Lists", "header"])
            );
            assert_eq!(
                value["position"],
                serde_json::json!({
                    "start": { "line": 18, "col": 0, "offset": 258 },
                    "end": { "line": 18, "col": 64, "offset": 322 },
                })
            );
            assert_eq!(value["lazySame"], true);
            assert_eq!(
                value["write"],
                serde_json::json!([" ", "2026-04-01", false, false])
            );
            assert_eq!(
                value["ship"],
                serde_json::json!([22, "People/Bob.md", true, false])
            );
        }

        #[test]
        fn dataviewjs_file_tags_follow_dataview() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            fs::write(
                vault_root.join("Tagged.md"),
                "---\ntags: \"front/one, #front/two shared\"\n---\nBody #body/deep/er and #shared.\n",
            )
            .expect("note should be written");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const eager = dv.page("Tagged").file;
                const lazy = dv.pages().where((page) => page.file.name === "Tagged")[0].file;
                ({
                  etags: eager.etags.array(),
                  tags: eager.tags.array(),
                  lazyTags: lazy.tags.array(),
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(
                value["etags"],
                serde_json::json!(["#body/deep/er", "#front/one", "#front/two", "#shared"])
            );
            let tags = serde_json::json!([
                "#body/deep/er",
                "#body/deep",
                "#body",
                "#front/one",
                "#front",
                "#front/two",
                "#shared"
            ]);
            assert_eq!(value["tags"], tags);
            assert_eq!(value["lazyTags"], tags);
        }

        #[test]
        fn dataviewjs_dates_and_durations_are_luxon_like() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r##"
                const page = dv.current();
                const { DateTime, Duration } = dv.luxon;
                const lateJanuary = DateTime.fromObject({ year: 2026, month: 1, day: 31 });
                ({
                  month: [page.month instanceof DateTime, page.month.toFormat("yyyy-MM-dd")],
                  duration: [Duration.isDuration(page.duration), page.duration.as("hours")],
                  durationFormat: page.duration.toFormat("d'd' h'h'"),
                  durationShift: page.duration.shiftTo("hours").toObject(),
                  durationText: String(dv.duration("90 min").shiftToAll().normalize().toHuman()),
                  durationNull: dv.duration("soon"),
                  mtimeIsDate: page.file.mtime instanceof DateTime,
                  monthArithmetic: lateJanuary.plus({ months: 1 }).toISODate(),
                  diffDays: lateJanuary.diff(DateTime.fromObject({ year: 2026, month: 1, day: 1 }), "days").days,
                  parse: [
                    dv.parse("2026-01-02").toISODate(),
                    dv.parse("3 days").days,
                    dv.parse("[[People/Bob|Bob]]").display,
                    dv.parse("#tag/child"),
                    dv.parse("true"),
                    dv.parse("1, 2"),
                    dv.parse('"quoted, text"'),
                    dv.parse("plain words"),
                    dv.parse("2026-13-01"),
                  ],
                  literal: dv.literal({ when: "2026-02-03", link: "[[People/Bob]]" }).when.month,
                  evaluated: dv.evaluate("this.month").value.toFormat("MMMM yyyy"),
                  dateFromLink: dv.date(dv.fileLink("Journal", false, "2026-05-06")).day,
                  durationformat: dv.func.durationformat(dv.duration("2 hours"), "m 'minutes'"),
                  rendered: dv.markdownList([page.month]),
                })
                "##,

                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(value["month"], serde_json::json!([true, "2026-04-01"]));
            assert_eq!(value["duration"], serde_json::json!([true, 27]));
            assert_eq!(value["durationFormat"], "1d 3h");
            assert_eq!(value["durationShift"], serde_json::json!({ "hours": 27 }));
            assert_eq!(
                value["durationText"],
                "0 years, 0 months, 0 weeks, 0 days, 1 hour, 30 minutes, 0 seconds, 0 milliseconds"
            );
            assert!(value["durationNull"].is_null());
            assert_eq!(value["mtimeIsDate"], true);
            assert_eq!(value["monthArithmetic"], "2026-02-28");
            assert_eq!(value["diffDays"], 30);
            assert_eq!(
                value["parse"],
                serde_json::json!([
                    "2026-01-02",
                    3,
                    "Bob",
                    "#tag/child",
                    true,
                    [1, 2],
                    "quoted, text",
                    "plain words",
                    "2026-13-01",
                ])
            );
            assert_eq!(value["literal"], 2);
            assert_eq!(value["evaluated"], "April 2026");
            assert_eq!(value["dateFromLink"], 6);
            assert_eq!(value["durationformat"], "120 minutes");
            assert_eq!(value["rendered"], "- April 01, 2026\n");
        }

        #[test]
        fn dataviewjs_links_are_dataview_link_objects() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const owner = dv.current().owner;
                const section = dv.sectionLink("People/Bob", "Contact & Notes!", true);
                const missing = dv.func.link("Nowhere");
                ({
                  owner: [owner.path, owner.type, owner.embed, owner.display ?? null],
                  ownerPage: dv.page(owner).file.name,
                  fileLink: String(dv.current().file.link),
                  section: [section.subpath, section.markdown(), section.obsidianLink()],
                  block: dv.blockLink("Projects/Alpha", "abc").markdown(),
                  missing: missing.path,
                  equal: dv.equal(owner, dv.fileLink("People/Bob.md", false, "Shown")),
                  sameLink: owner.equals(dv.fileLink("People/Bob.md")),
                  toObject: owner.withDisplay("Bob").toObject(),
                  embed: dv.func.embed(owner).markdown(),
                  meta: dv.func.meta(owner),
                  clonedIsLink: typeof dv.clone({ owner }).owner.obsidianLink,
                  outlinkDisplays: dv.current().file.outlinks.map((link) => link.display).distinct().sort().array(),
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            let value = result.value.expect("script value");
            assert_eq!(
                value["owner"],
                serde_json::json!(["People/Bob.md", "file", false, null])
            );
            assert_eq!(value["ownerPage"], "Bob");
            assert_eq!(value["fileLink"], "[[Dashboard.md|Dashboard]]");
            assert_eq!(
                value["section"],
                serde_json::json!([
                    "Contact Notes",
                    "![[People/Bob#Contact Notes|Bob > Contact Notes]]",
                    "People/Bob#Contact Notes"
                ])
            );
            assert_eq!(value["block"], "[[Projects/Alpha#^abc|Alpha > abc]]");
            assert_eq!(value["missing"], "Nowhere");
            assert_eq!(value["equal"], true);
            assert_eq!(value["sameLink"], true);
            assert_eq!(
                value["toObject"],
                serde_json::json!({
                    "path": "People/Bob.md",
                    "type": "file",
                    "display": "Bob",
                    "embed": false,
                })
            );
            assert_eq!(value["embed"], "![[People/Bob.md|Bob]]");
            assert_eq!(
                value["meta"],
                serde_json::json!({
                    "display": null,
                    "embed": false,
                    "path": "People/Bob.md",
                    "subpath": null,
                    "type": "file",
                })
            );
            assert_eq!(value["clonedIsLink"], "function");
            assert!(value["outlinkDisplays"]
                .as_array()
                .is_some_and(|displays| displays
                    .iter()
                    .any(|display| display == "Projects/Alpha")));
        }

        #[test]
        fn dataviewjs_reports_unhandled_rejections_and_awaits_views() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            fs::write(
                vault_root.join("views/awaiting.js"),
                "const markdown = await dv.tryQueryMarkdown('LIST FROM \"Projects\" SORT file.name ASC');\n\
                 dv.paragraph(input.prefix + markdown.split('\\n').length);\n",
            )
            .expect("view should be written");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let options = || DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Fs),
                ..DataviewJsEvalOptions::default()
            };

            let error = evaluate_dataview_js_with_options(
                &paths,
                "(async () => { throw new Error('lost in a promise'); })();",
                Some("Dashboard.md"),
                options(),
            )
            .expect_err("an unhandled rejection should fail the evaluation");
            assert!(
                error.to_string().contains("lost in a promise"),
                "unexpected error: {error}"
            );

            let handled = evaluate_dataview_js_with_options(
                &paths,
                "dv.tryQuery('NOT A QUERY').catch(() => dv.paragraph('caught'));\n\
                 dv.view('views/awaiting.js', { prefix: 'lines: ' });",
                Some("Dashboard.md"),
                options(),
            )
            .expect("handled rejections and awaiting views should evaluate");

            let awaited = evaluate_dataview_js_with_options(
                &paths,
                "const result = await dv.query('LIST FROM \"Projects\"');\n\
                 result.successful",
                Some("Dashboard.md"),
                options(),
            )
            .expect("top-level await should evaluate");
            assert_eq!(awaited.value, Some(Value::Bool(true)));
            let promised = evaluate_dataview_js_with_options(
                &paths,
                "dv.io.load('views/awaiting.js').then((text) => text.startsWith('const'))",
                Some("Dashboard.md"),
                options(),
            )
            .expect("a promise completion value should settle");
            assert_eq!(promised.value, Some(Value::Bool(true)));
            assert_eq!(
                handled.outputs,
                vec![
                    DataviewJsOutput::Paragraph {
                        text: "caught".to_string(),
                    },
                    DataviewJsOutput::Paragraph {
                        text: "lines: 3".to_string(),
                    },
                ]
            );
        }

        #[test]
        fn dataviewjs_supports_query_markdown_and_return_values() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                dv.tryQueryMarkdown('LIST FROM "Projects" SORT file.name ASC').then(dv.paragraph);
                dv.current().status
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.outputs,
                vec![DataviewJsOutput::Paragraph {
                    text: "- [[Projects/Alpha.md|Alpha]]\n- [[Projects/Beta.md|Beta]]\n"
                        .to_string(),
                }]
            );
            assert_eq!(result.value, Some(Value::String("draft".to_string())));
        }

        #[test]
        fn dataviewjs_exposes_vault_daily_namespace_and_events() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
            fs::write(
                vault_root.join(".vulcan/config.local.toml"),
                "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
            )
            .expect("config should be written");
            fs::create_dir_all(vault_root.join("Journal/Daily"))
                .expect("daily directory should be created");
            fs::write(
                vault_root.join("Journal/Daily/2026-04-03.md"),
                "# 2026-04-03\n\nstatus: active\n\n## Schedule\n- 09:00-10:00 Team standup @location(Zoom)\n- 14:00 Dentist #personal\n",
            )
            .expect("first daily note should be written");
            fs::write(
                vault_root.join("Journal/Daily/2026-04-04.md"),
                "# 2026-04-04\n\n## Schedule\n- all-day Company offsite\n",
            )
            .expect("second daily note should be written");

            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const note = vault.daily.get("2026-04-03");
                const range = vault.daily.range("2026-04-03", "2026-04-04");
                const events = vault.events({ from: "2026-04-03", to: "2026-04-04" });
                dv.table(
                  ["name", "range", "events", "location", "source"],
                  [[
                    note.file.name,
                    range.length,
                    events.length,
                    note.events[0].metadata.location,
                    events[2].path
                  ]]
                );
                "#,
                Some("Journal/Daily/2026-04-03.md"),
            )
            .expect("DataviewJS should evaluate");

            assert!(
                matches!(&result.outputs[0], DataviewJsOutput::Table { headers, rows }
                if headers == &vec![
                    "name".to_string(),
                    "range".to_string(),
                    "events".to_string(),
                    "location".to_string(),
                    "source".to_string(),
                ]
                && rows == &vec![vec![
                    Value::String("2026-04-03".to_string()),
                    Value::from(2),
                    Value::from(3),
                    Value::String("Zoom".to_string()),
                    Value::String("Journal/Daily/2026-04-04.md".to_string()),
                ]])
            );
        }

        #[test]
        fn dataviewjs_exposes_vault_note_query_search_graph_and_help_helpers() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const note = vault.note("Projects/Alpha");
                const notes = vault.notes('"Projects"').sortBy((row) => row.file.name);
                const query = vault.query('TABLE status FROM "Projects" SORT file.name ASC');
                const search = vault.search("Alpha", { limit: 1 });
                const path = vault.graph.shortestPath("Dashboard", "Projects/Beta");
                console.log(help(vault.search));
                dv.table(
                  ["note", "notes", "query", "search", "path"],
                  [[
                    note.file.name,
                    notes.length,
                    query.result_count,
                    search.hits.length,
                    path.path.length
                  ]]
                );
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            match &result.outputs[0] {
                DataviewJsOutput::Paragraph { text } => {
                    assert!(text.contains(
                        "vault.search(query?: string, opts?: { limit?: number }): SearchReport | SearchBuilder"
                    ));
                    assert!(text.contains("Parameters:"));
                    assert!(text.contains("Example:"));
                    assert!(text.contains("See also: vault.notes(), vault.query()"));
                }
                other => panic!("expected paragraph output, got {other:?}"),
            }
            match &result.outputs[1] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(
                        headers,
                        &vec![
                            "note".to_string(),
                            "notes".to_string(),
                            "query".to_string(),
                            "search".to_string(),
                            "path".to_string(),
                        ]
                    );
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0][0], Value::String("Alpha".to_string()));
                    assert!(rows[0][1].as_i64().is_some_and(|value| value >= 2));
                    assert!(rows[0][2].as_i64().is_some_and(|value| value >= 2));
                    assert_eq!(rows[0][3], Value::from(1));
                    assert_ne!(rows[0][4], Value::Null);
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_exposes_custom_tool_authoring_helpers() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r##"
                const note = vault.note("Projects/Alpha");
                const section = note.section();
                const query = vault.query().path("Projects").where("status", "=", "active").limit(2);
                const search = vault.search().text("Alpha").path("Projects").limit(1);
                const plan = vault.plan({ dry_run: true })
                  .append("Projects/Alpha", "- [ ] Planned")
                  .diff("Projects/Alpha", "old", "new")
                  .result();
                const permissions = vulcan.permissions();
                const tmp = vulcan.tempDir();
                tmp.writeJson("input.json", { ok: true });
                ({
                  section_has_content: String(section.content ?? "").includes("#project"),
                  query_count: query.run().result_count,
                  search_hits: search.run().hits.length,
                  plan_dry_run: plan.dry_run,
                  plan_paths: plan.changed_paths,
                  diff_has_plus: vault.diffText("a", "b", { path: "X.md" }).includes("+b"),
                  daily_path: vault.daily.path("2026-04-03"),
                  can_write: permissions.canWrite("Projects/Alpha.md"),
                  tmp_ok: tmp.readJson("input.json").ok,
                  result_ok: tool.result().summary("done").changedPath("Projects/Alpha.md").ok().ok
                })
                "##,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: None,
                    deterministic_static: true,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("DataviewJS helper surface should evaluate");

            let value = result.value.expect("helper result should be serializable");
            assert_eq!(value["section_has_content"], Value::Bool(true));
            assert!(value["query_count"]
                .as_i64()
                .is_some_and(|count| count >= 1));
            assert_eq!(value["search_hits"], Value::from(1));
            assert_eq!(value["plan_dry_run"], Value::Bool(true));
            assert_eq!(
                value["plan_paths"],
                Value::Array(vec![Value::String("Projects/Alpha".to_string())])
            );
            assert_eq!(value["diff_has_plus"], Value::Bool(true));
            assert_eq!(
                value["daily_path"],
                Value::String("Journal/Daily/2026-04-03.md".to_string())
            );
            assert_eq!(value["can_write"], Value::Bool(true));
            assert_eq!(value["tmp_ok"], Value::Bool(true));
            assert_eq!(value["result_ok"], Value::Bool(true));
        }

        #[test]
        fn dataviewjs_rejects_paths_outside_the_vault() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"dv.io.load("../outside.md")"#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("DataviewJS should reject vault escapes");
            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("must stay inside the vault root")
            ));
        }

        #[test]
        fn dataviewjs_io_load_respects_the_selected_read_profile() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            fs::write(
                vault_root.join(".vulcan/config.toml"),
                "[permissions.profiles.blind]\nread = \"none\"\n",
            )
            .expect("permission config should be written");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"dv.io.load("Projects/Alpha.md")"#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: Some("blind".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("denied file should not be readable");

            assert!(error.to_string().contains("permission denied"));
        }

        #[test]
        fn dataviewjs_page_universe_keeps_read_scope_after_mutation_reloads() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            for (path, contents) in [
                ("Public/Target.md", "# Target\n"),
                ("Public/Visible.md", "[[Public/Target]]\n"),
                (
                    "Public/Secret.md",
                    "---\ntags: [secret]\n---\n[[Public/Target]]\n",
                ),
                ("Private/Hidden.md", "[[Public/Target]]\n"),
            ] {
                fs::create_dir_all(vault_root.join(path).parent().unwrap()).unwrap();
                fs::write(vault_root.join(path), contents).unwrap();
            }
            fs::create_dir_all(vault_root.join(".vulcan")).unwrap();
            fs::write(
                vault_root.join(".vulcan/config.toml"),
                concat!(
                    "[permissions.profiles.scoped]\n",
                    "read = { allow = [\"folder:Public/**\"], deny = [\"tag:secret\"] }\n",
                    "write = { allow = [\"folder:Public/**\"] }\n",
                    "execute = \"allow\"\n",
                ),
            )
            .unwrap();
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r#"
                const snapshot = () => ({
                  pages: dv.pages().file.path.array().sort(),
                  inlinks: Array.from(dv.page("Public/Target").file.inlinks, link => String(link.path ?? link)).sort(),
                });
                const before = snapshot();
                vault.create("Public/New", { content: "[[Public/Target]]\n" });
                ({ before, after: snapshot() })
                "#,
                Some("Public/Visible.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: Some("scoped".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("scoped script should run");

            let value = result.value.expect("script value");
            assert_eq!(
                value["before"],
                serde_json::json!({
                    "pages": ["Public/Target.md", "Public/Visible.md"],
                    "inlinks": ["Public/Visible.md"],
                })
            );
            assert_eq!(
                value["after"],
                serde_json::json!({
                    "pages": ["Public/New.md", "Public/Target.md", "Public/Visible.md"],
                    "inlinks": ["Public/New.md", "Public/Visible.md"],
                })
            );
        }

        #[cfg(unix)]
        #[test]
        fn dataviewjs_io_load_rejects_symlinks_to_outside_files() {
            use std::os::unix::fs::symlink;

            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let outside = temp_dir.path().join("secret.md");
            fs::write(&outside, "outside secret").expect("outside file should be written");
            symlink(&outside, vault_root.join("linked.md")).expect("symlink should be created");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"dv.io.load("linked.md")"#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("symlinked file should not be readable");

            assert!(!error.to_string().contains("outside secret"));
        }

        #[test]
        fn dataviewjs_times_out_infinite_loops() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
            fs::write(
                vault_root.join(".vulcan/config.toml"),
                "[dataview]\njs_timeout_seconds = 1\n",
            )
            .expect("config should be written");

            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let started = Instant::now();
            let error = evaluate_dataview_js_query(&paths, "while (true) {}", Some("Dashboard.md"))
                .expect_err("DataviewJS should time out");

            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("timed out after 1 second")
            ));
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn dataviewjs_permission_profile_caps_runtime_timeout() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
            fs::write(
                vault_root.join(".vulcan/config.toml"),
                r#"[dataview]
js_timeout_seconds = 5

[permissions.profiles.fast]
read = "all"
execute = "allow"
cpu_limit_ms = 25
"#,
            )
            .expect("config should be written");

            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let started = Instant::now();
            let error = evaluate_dataview_js_with_options(
                &paths,
                "while (true) {}",
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Strict),
                    permission_profile: Some("fast".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("permission profile should cap JS runtime");

            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("timed out after 25 ms")
            ));
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        #[test]
        fn dataviewjs_session_preserves_values_between_evaluations() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let session = DataviewJsSession::new(
                &paths,
                Some("Dashboard.md"),
                DataviewJsEvalOptions::default(),
            )
            .expect("session should initialize");
            session
                .evaluate("const project = vault.note('Projects/Alpha').file.name;")
                .expect("first evaluation should succeed");
            let result = session
                .evaluate("project")
                .expect("second evaluation should reuse the same context");

            assert_eq!(result.value, Some(Value::String("Alpha".to_string())));
        }

        #[test]
        fn dataviewjs_note_class_exposes_details_and_relationships() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r#"
                const dashboard = vault.note("Dashboard");
                const alpha = vault.note("Projects/Alpha");
                dv.table(
                  ["name", "content", "headings", "fields", "links", "backlinks", "neighbors"],
                  [[
                    dashboard.name,
                    dashboard.content.includes("priority:: 2"),
                    dashboard.headings.length,
                    dashboard.dataview_fields.length,
                    dashboard.links().length,
                    alpha.backlinks().length,
                    dashboard.neighbors(1).length
                  ]]
                );
                "#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("DataviewJS should evaluate");

            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(
                        headers,
                        &vec![
                            "name".to_string(),
                            "content".to_string(),
                            "headings".to_string(),
                            "fields".to_string(),
                            "links".to_string(),
                            "backlinks".to_string(),
                            "neighbors".to_string(),
                        ]
                    );
                    assert_eq!(rows[0][0], Value::String("Dashboard".to_string()));
                    assert_eq!(rows[0][1], Value::Bool(true));
                    assert!(rows[0][2].as_i64().is_some_and(|value| value >= 2));
                    assert!(rows[0][3].as_i64().is_some_and(|value| value >= 6));
                    assert!(rows[0][4].as_i64().is_some_and(|value| value >= 2));
                    assert!(rows[0][5].as_i64().is_some_and(|value| value >= 1));
                    assert!(rows[0][6].as_i64().is_some_and(|value| value >= 1));
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_note_content_is_raw_markdown_and_html_is_rendered() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r#"
                const note = vault.note("Dashboard");
                dv.table(
                  ["raw", "html"],
                  [[
                    note.content.startsWith("---\nstatus: draft"),
                    note.html.includes("Lists</h2>") && !note.html.includes("status: draft")
                  ]]
                );
                "#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("DataviewJS should evaluate");

            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(headers, &vec!["raw".to_string(), "html".to_string()]);
                    assert_eq!(rows, &vec![vec![Value::Bool(true), Value::Bool(true)]]);
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_note_outline_and_partial_reads_share_cli_selectors() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_with_options(
                &paths,
                r#"
                const note = vault.note("Dashboard");
                const outline = note.outline();
                const tasks = outline.sections.find((section) => section.heading === "Tasks");
                const selection = note.read({ section: tasks.id, match: "Write", context: 1 });
                dv.table(
                  ["sections", "section", "start", "content", "before", "after"],
                  [[
                    outline.sections.length,
                    selection.section_id,
                    selection.line_spans[0].start_line,
                    selection.content.includes("Write docs") && selection.content.includes("Ship release"),
                    selection.has_more_before,
                    selection.has_more_after
                  ]]
                );
                "#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("DataviewJS should evaluate");

            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(
                        headers,
                        &vec![
                            "sections".to_string(),
                            "section".to_string(),
                            "start".to_string(),
                            "content".to_string(),
                            "before".to_string(),
                            "after".to_string(),
                        ]
                    );
                    assert!(rows[0][0].as_i64().is_some_and(|value| value >= 2));
                    assert_eq!(rows[0][1], Value::String("tasks@22".to_string()));
                    assert_eq!(rows[0][2], Value::from(22));
                    assert_eq!(rows[0][3], Value::Bool(true));
                    assert_eq!(rows[0][4], Value::Bool(true));
                    assert_eq!(rows[0][5], Value::Bool(true));
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_enforces_fs_sandbox_and_supports_transactions() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_with_options(
                &paths,
                r##"vault.create("Scratch", { content: "# Scratch" })"##,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Strict),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("strict sandbox should reject writes");
            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("requires --sandbox fs or higher")
            ));

            let session = DataviewJsSession::new(
                &paths,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("fs sandbox session should initialize");
            let result = session
                .evaluate(
                    r###"
                    const created = vault.create("Scratch", {
                      content: "Body",
                      frontmatter: { status: "draft" }
                    });
                    vault.create("Shorthand", "# Shorthand\n");
                    vault.plan().create("PlanShorthand", "In a plan\n").apply();
                    vault.append("Scratch", "Follow-up", { heading: "## Log" });
                    vault.update("Scratch", "owner", "alice");
                    vault.unset("Scratch", "status");
                    vault.note("Scratch").properties.set("phase", "active");
                    vault.mutation()
                      .appendNote("Scratch", "From plan")
                      .update("Scratch", "planned", true)
                      .result();
                    created.name
                    "###,
                )
                .expect("fs sandbox mutations should succeed");
            assert_eq!(result.value, Some(Value::String("Scratch".to_string())));

            let scratch = fs::read_to_string(vault_root.join("Scratch.md"))
                .expect("scratch note should exist");
            assert!(scratch.contains("owner: alice"));
            assert!(scratch.contains("phase: active"));
            assert!(scratch.contains("planned: true"));
            assert!(!scratch.contains("status: draft"));
            assert!(scratch.contains("## Log"));
            assert!(scratch.contains("Follow-up"));
            assert!(scratch.contains("From plan"));
            // A string second argument is the content.
            assert_eq!(
                fs::read_to_string(vault_root.join("Shorthand.md")).unwrap(),
                "# Shorthand\n"
            );
            assert_eq!(
                fs::read_to_string(vault_root.join("PlanShorthand.md")).unwrap(),
                "In a plan\n"
            );

            let error = session
                .evaluate(
                    r#"
                    vault.transaction((tx) => {
                      tx.create("Temp", { content: "temporary" });
                      throw new Error("rollback");
                    });
                    "#,
                )
                .expect_err("transaction should roll back on error");
            assert!(matches!(
                error,
                DataviewJsError::Message(message) if message.contains("rollback")
            ));
            assert!(!vault_root.join("Temp.md").exists());
        }

        #[test]
        fn dataviewjs_inbox_refuses_to_overwrite_an_unreadable_note() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan directory");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let original = b"# Inbox\n\xff\xfe keep me\n".to_vec();
            fs::write(vault_root.join("Inbox.md"), &original).expect("inbox note");
            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"vault.inbox("new entry")"#,
                None,
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("an unreadable inbox must not be replaced");
            assert!(matches!(
                error,
                DataviewJsError::Message(message) if message.contains("cannot read")
            ));
            assert_eq!(
                fs::read(vault_root.join("Inbox.md")).expect("inbox"),
                original
            );
        }

        #[test]
        fn dataviewjs_routes_implicit_and_explicit_transaction_commits() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan directory");
            fs::write(vault_root.join("Alpha.md"), "# Alpha\n").expect("alpha note");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let committer = Arc::new(RecordingMutationCommitter {
                calls: Mutex::new(Vec::new()),
                reject: false,
            });

            evaluate_dataview_js_with_options(
                &paths,
                r##"
                vault.append("Alpha", "implicit");
                vault.transaction((tx) => {
                  tx.append("Alpha", "explicit");
                  tx.create("Beta", { content: "# Beta" });
                });
                "##,
                None,
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    mutation_committer: Some(committer.clone()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("script writes should commit");

            let calls = committer.calls.lock().expect("committer calls");
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].len(), 1);
            assert_eq!(calls[0][0].path, "Alpha.md");
            assert_eq!(calls[1].len(), 2);
            assert_eq!(calls[1][0].path, "Alpha.md");
            assert_eq!(calls[1][1].path, "Beta.md");
            assert!(fs::read_to_string(vault_root.join("Alpha.md"))
                .expect("alpha note")
                .contains("explicit"));
            assert!(vault_root.join("Beta.md").exists());
        }

        #[test]
        fn dataviewjs_refactors_commit_their_plan_through_the_committer() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan directory");
            fs::write(
                vault_root.join("Alpha.md"),
                "---\naliases: [Old]\n---\n# Alpha\n",
            )
            .expect("alpha note");
            fs::write(vault_root.join("Beta.md"), "See [[Alpha#Alpha]].\n").expect("beta");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let run = |reject, script: &str| {
                let committer = Arc::new(RecordingMutationCommitter {
                    calls: Mutex::new(Vec::new()),
                    reject,
                });
                let result = evaluate_dataview_js_with_options(
                    &paths,
                    script,
                    None,
                    DataviewJsEvalOptions {
                        sandbox: Some(JsRuntimeSandbox::Fs),
                        mutation_committer: Some(committer.clone()),
                        ..DataviewJsEvalOptions::default()
                    },
                );
                let calls = committer.calls.lock().expect("calls").clone();
                (result, calls)
            };

            // A rejected refactor writes nothing.
            let (result, calls) = run(true, r#"vault.refactor.renameAlias("Alpha", "Old", "New")"#);
            assert!(result.is_err());
            assert_eq!(calls.len(), 1);
            assert!(fs::read_to_string(vault_root.join("Alpha.md"))
                .unwrap()
                .contains("[Old]"));

            // Unhandled changes apply as an ordinary batch, across notes.
            let (result, calls) = run(
                false,
                r#"vault.refactor.renameAlias("Alpha", "Old", "New");
                   vault.refactor.renameHeading("Alpha", "Alpha", "Top");"#,
            );
            result.expect("refactors apply");
            assert_eq!(calls.len(), 2);
            assert_eq!(
                calls[1]
                    .iter()
                    .map(|change| change.path.as_str())
                    .collect::<Vec<_>>(),
                ["Alpha.md", "Beta.md"]
            );
            assert!(fs::read_to_string(vault_root.join("Alpha.md"))
                .unwrap()
                .contains("New"));
            assert_eq!(
                fs::read_to_string(vault_root.join("Beta.md")).unwrap(),
                "See [[Alpha#Top]].\n"
            );

            // Inside an explicit transaction they are still refused.
            let (result, _) = run(
                false,
                r#"vault.transaction(() => vault.refactor.renameAlias("Alpha", "New", "Newer"))"#,
            );
            assert!(result.is_err());
        }

        #[test]
        fn dataviewjs_commit_rejection_restores_originals() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan directory");
            fs::write(vault_root.join("Alpha.md"), "# Alpha\n").expect("alpha note");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let committer = Arc::new(RecordingMutationCommitter {
                calls: Mutex::new(Vec::new()),
                reject: true,
            });

            let error = evaluate_dataview_js_with_options(
                &paths,
                r##"vault.transaction((tx) => {
                  tx.append("Alpha", "rejected");
                  tx.create("Beta", { content: "# Beta" });
                })"##,
                None,
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    mutation_committer: Some(committer),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("committer should reject the transaction");

            assert!(error.to_string().contains("proposed script write rejected"));
            assert_eq!(
                fs::read_to_string(vault_root.join("Alpha.md")).expect("alpha note"),
                "# Alpha\n"
            );
            assert!(!vault_root.join("Beta.md").exists());
        }

        #[test]
        fn dataviewjs_static_mode_rejects_wall_clock_helpers_and_host_side_effects() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let exec_argv =
                serde_json::to_string(&test_host_exec_argv(&test_host_output_command("alpha")))
                    .expect("argv json should serialize");
            let shell_command =
                serde_json::to_string(&test_host_output_command("alpha")).expect("shell string");
            let cases = vec![
                ("Date.now()".to_string(), "wall-clock time via Date.now()"),
                ("new Date()".to_string(), "wall-clock time via new Date()"),
                (
                    "VulcanDateTime.now()".to_string(),
                    "wall-clock time via VulcanDateTime.now()",
                ),
                (
                    "vault.daily.today()".to_string(),
                    "wall-clock time via vault.daily.today()",
                ),
                (
                    r#"vault.daily.get("today")"#.to_string(),
                    "wall-clock time via vault.daily.get()",
                ),
                (
                    "vault.events()".to_string(),
                    "wall-clock time via vault.events()",
                ),
                (
                    format!("host.exec({exec_argv})"),
                    "host execution via host.exec()",
                ),
                (
                    format!("host.shell({shell_command})"),
                    "shell execution via host.shell()",
                ),
                (
                    r#"web.fetch("https://example.test")"#.to_string(),
                    "network access via web.fetch()",
                ),
            ];

            for (source, expected) in cases {
                let error = evaluate_dataview_js_with_options(
                    &paths,
                    &source,
                    Some("Dashboard.md"),
                    DataviewJsEvalOptions {
                        timeout: None,
                        sandbox: Some(JsRuntimeSandbox::Strict),
                        deterministic_static: true,
                        ..DataviewJsEvalOptions::default()
                    },
                )
                .expect_err("static DataviewJS should reject non-deterministic helpers");
                assert!(
                    error.to_string().contains(expected),
                    "expected `{expected}` in `{error}` for source `{source}`"
                );
            }
        }

        #[test]
        fn dataviewjs_static_mode_rejects_filesystem_writes_and_transactions() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            for (source, expected) in [
                (
                    r##"vault.create("Scratch", { content: "# Scratch" })"##,
                    "filesystem writes via vault.create()",
                ),
                (
                    "vault.transaction(() => null)",
                    "filesystem writes via vault.transaction()",
                ),
            ] {
                let error = evaluate_dataview_js_with_options(
                    &paths,
                    source,
                    Some("Dashboard.md"),
                    DataviewJsEvalOptions {
                        timeout: None,
                        sandbox: Some(JsRuntimeSandbox::Strict),
                        deterministic_static: true,
                        ..DataviewJsEvalOptions::default()
                    },
                )
                .expect_err("static DataviewJS should reject filesystem writes");
                assert!(
                    error.to_string().contains(expected),
                    "expected `{expected}` in `{error}` for source `{source}`"
                );
            }
        }

        #[allow(clippy::too_many_lines)]
        #[test]
        fn dataviewjs_web_helpers_require_net_sandbox_and_can_fetch() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
            let address = listener
                .local_addr()
                .expect("listener should have a local address");
            let base_url = format!("http://{address}");
            let handle = thread::spawn(move || {
                for _ in 0..3 {
                    let (mut stream, _) = listener.accept().expect("connection should be accepted");
                    let mut buffer = [0_u8; 4096];
                    let read = stream
                        .read(&mut buffer)
                        .expect("request should be readable");
                    let request = String::from_utf8_lossy(&buffer[..read]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let (content_type, body) = match path {
                        "/robots.txt" => ("text/plain", "User-agent: *\nAllow: /\n".to_string()),
                        path if path.starts_with("/search") => (
                            "text/html",
                            r#"<!doctype html><html><body>
<div class="result">
  <a class="result__a" href="https://example.test/alpha">Alpha</a>
  <a class="result__snippet">Alpha result</a>
</div>
</body></html>"#
                                .to_string(),
                        ),
                        _ => (
                            "text/html",
                            "<html><body><article><h1>Hello</h1><p>Alpha page</p></article></body></html>"
                                .to_string(),
                        ),
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("response should be writable");
                }
            });
            fs::write(
                vault_root.join(".vulcan/config.local.toml"),
                format!("[web.search]\nbase_url = \"{base_url}/search\"\n"),
            )
            .expect("config should be written");

            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_with_options(
                &paths,
                r#"web.fetch("http://127.0.0.1")"#,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Strict),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("strict sandbox should reject network");
            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("requires --sandbox net or higher")
            ));

            let result = evaluate_dataview_js_with_options(
                &paths,
                &format!(
                    r#"
                    const search = web.search("Alpha", {{ limit: 1 }});
                    const fetched = web.fetch("{base_url}/article", {{ mode: "markdown" }});
                    dv.table(["title", "status", "content"], [[search.results[0].title, fetched.status, fetched.content.includes("Alpha page")]]);
                    "#
                ),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: None,
                    sandbox: Some(JsRuntimeSandbox::Net),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("net sandbox should allow web helpers");

            handle.join().expect("server thread should finish");

            match &result.outputs[0] {
                DataviewJsOutput::Table { rows, .. } => {
                    assert_eq!(rows[0][0], Value::String("Alpha".to_string()));
                    assert_eq!(rows[0][1], Value::from(200));
                    assert_eq!(rows[0][2], Value::Bool(true));
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_data_arrays_follow_dataview_semantics() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const numbers = dv.array([3, 1, 2]);
                const nodes = dv.array([{ name: "a", kids: [{ name: "b", kids: [] }] }]);
                const rows = dv.array([{ k: "b" }, { k: "a" }, { k: "b" }, { k: null }]);
                ({
                  keyArgs: numbers.sort((value, index, values) => [typeof index, values.length].join()).length,
                  desc: numbers.sort((value) => value, "desc").array(),
                  comparator: numbers.sort((value) => value, "asc", (a, b) => b - a).array(),
                  firstLast: [numbers.first(), numbers.last(), dv.array([]).first()],
                  stats: [numbers.sum(), numbers.avg(), numbers.min(), numbers.max()],
                  to: nodes.to("kids").map((kid) => kid.name).array(),
                  into: rows.into("k").array(),
                  expand: nodes.expand("kids").map((node) => node.name).array().sort(),
                  distinct: rows.distinct((row) => row.k).length,
                  groups: rows.groupBy((row) => row.k).map((group) => [group.key, group.rows.length]).array(),
                  indexOf: [numbers.indexOf(2), numbers.indexOf(3, 1), numbers.findIndex((value) => value > 0, 1)],
                  text: numbers.toString(),
                })
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.value,
                Some(serde_json::json!({
                    "keyArgs": 3,
                    "desc": [3, 2, 1],
                    "comparator": [3, 2, 1],
                    "firstLast": [3, 2, null],
                    "stats": [6, 2, 1, 3],
                    "to": ["b"],
                    "into": ["b", "a", "b"],
                    "expand": ["a", "b"],
                    "distinct": 3,
                    "groups": [[null, 1], ["a", 1], ["b", 2]],
                    "indexOf": [2, -1, 1],
                    "text": "[3, 1, 2]",
                }))
            );
        }

        #[test]
        fn dataviewjs_date_accepts_shorthands_and_static_mode_rejects_them() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let source = r#"
                const day = 86400000;
                [
                  dv.date("tomorrow").toMillis() - dv.date("today").toMillis() === day,
                  dv.date("start-of-month").toMillis() === dv.date("som").toMillis(),
                  dv.date("today").toFormat("yyyy-MM-dd") === new Date().toISOString().slice(0, 10),
                  dv.date("not a date"),
                ]
            "#;

            let result = evaluate_dataview_js_query(&paths, source, Some("Dashboard.md"))
                .expect("DataviewJS should evaluate");
            assert_eq!(
                result.value,
                Some(serde_json::json!([true, true, true, null]))
            );

            let error = evaluate_dataview_js_with_options(
                &paths,
                "dv.date('today')",
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    deterministic_static: true,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("static mode should reject the clock");
            assert!(error.to_string().contains("wall-clock time"), "{error}");
        }

        #[test]
        fn dataviewjs_console_output_is_hidden_only_while_rendering() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let source = "console.log('log'); console.warn('warn'); dv.paragraph('shown')";
            let run = |read_only| {
                evaluate_dataview_js_with_options(
                    &paths,
                    source,
                    Some("Dashboard.md"),
                    DataviewJsEvalOptions {
                        read_only,
                        ..DataviewJsEvalOptions::default()
                    },
                )
                .expect("DataviewJS should evaluate")
                .outputs
            };
            let paragraph = |text: &str| DataviewJsOutput::Paragraph {
                text: text.to_string(),
            };

            assert_eq!(
                run(false),
                vec![paragraph("log"), paragraph("warn"), paragraph("shown")]
            );
            assert_eq!(run(true), vec![paragraph("shown")]);
        }

        #[test]
        fn dataviewjs_file_arrays_are_data_arrays() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const current = dv.current().file;
                const paged = dv.pages().where((p) => p.file.name === "Dashboard").array()[0].file;
                [
                  current.lists.where((item) => item.kind === "note").length,
                  current.tasks.where((task) => task.due).length,
                  paged.lists.where((item) => item.kind === "note").length,
                  JSON.stringify(current.tags) === JSON.stringify(current.tags.array()),
                ]
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.value, Some(serde_json::json!([1, 1, 1, true])));
        }

        #[test]
        fn dataviewjs_omits_fetch_but_keeps_eval_and_function() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                dv.paragraph(
                  [
                    typeof fetch === "undefined" ? "no-fetch" : "fetch",
                    eval("1 + 1") === 2 ? "eval" : "no-eval",
                    new Function("return 6 / 3")() === 2 ? "function" : "no-function"
                  ].join(", ")
                )
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.outputs,
                vec![DataviewJsOutput::Paragraph {
                    text: "no-fetch, eval, function".to_string(),
                }]
            );
        }

        #[test]
        fn dataviewjs_supports_dv_func_and_luxon_helpers() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                const dt = dv.luxon.DateTime.fromISO("2026-04-03").plus(
                  dv.luxon.Duration.fromObject({ days: 2 })
                );
                const lowered = dv.func.lower(["ALPHA", "BETA"]).join(", ");
                const ownerPath = dv.func.meta(dv.func.link("People/Bob")).path;
                dv.table(
                  ["lowered", "contains", "formatted", "owner", "plus"],
                  [[
                    lowered,
                    dv.func.contains("Release notes", "notes"),
                    dv.func.dateformat(dv.date("2026-04-03"), "yyyy-MM-dd"),
                    ownerPath,
                    dt.toFormat("yyyy-MM-dd")
                  ]]
                );
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.outputs.len(), 1);
            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(
                        headers,
                        &vec![
                            "lowered".to_string(),
                            "contains".to_string(),
                            "formatted".to_string(),
                            "owner".to_string(),
                            "plus".to_string(),
                        ]
                    );
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0][0], Value::String("alpha, beta".to_string()));
                    assert_eq!(rows[0][1], Value::Bool(true));
                    assert_eq!(rows[0][2], Value::String("2026-04-03".to_string()));
                    assert_eq!(rows[0][3], Value::String("People/Bob.md".to_string()));
                    assert_eq!(rows[0][4], Value::String("2026-04-05".to_string()));
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_exposes_formattable_dates_for_dv_date_and_file_day() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            fs::create_dir_all(vault_root.join("Journal")).expect("journal dir should exist");
            fs::write(vault_root.join("Journal/2026-04-03.md"), "# Daily\n")
                .expect("daily note should be written");
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"
                dv.table(
                  ["fileDay", "parsed"],
                  [[
                    dv.page("Journal/2026-04-03").file.day.toFormat("yyyy-MM-dd"),
                    dv.date("2026-04-04").toFormat("yyyy-MM-dd")
                  ]]
                );
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(result.outputs.len(), 1);
            match &result.outputs[0] {
                DataviewJsOutput::Table { headers, rows } => {
                    assert_eq!(headers, &vec!["fileDay".to_string(), "parsed".to_string()]);
                    assert_eq!(
                        rows,
                        &vec![vec![
                            Value::String("2026-04-03".to_string()),
                            Value::String("2026-04-04".to_string())
                        ]]
                    );
                }
                other => panic!("expected table output, got {other:?}"),
            }
        }

        #[test]
        fn dataviewjs_plain_serializes_functions_as_string() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            // Bare function reference should no longer cause a type conversion error.
            let result = evaluate_dataview_js_query(&paths, "help", Some("Dashboard.md"))
                .expect("evaluating a bare function should succeed");
            let value = result.value.expect("should have a value");
            assert_eq!(
                value,
                Value::String("[function help]".to_string()),
                "function should serialize as [function name]"
            );
        }

        #[test]
        fn dataviewjs_help_no_arg_returns_welcome_message() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(&paths, "help()", Some("Dashboard.md"))
                .expect("help() should succeed");
            let value = result.value.expect("help() should return a value");
            let text = value.as_str().expect("help() should return a string");
            assert!(
                text.contains("Vulcan JS Runtime"),
                "help() should include welcome heading, got: {text}"
            );
            assert!(
                text.contains("vault"),
                "help() should mention vault, got: {text}"
            );
            assert!(text.contains("dv"), "help() should mention dv, got: {text}");
        }

        #[test]
        fn dataviewjs_help_vault_returns_api_overview() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(&paths, "help(vault)", Some("Dashboard.md"))
                .expect("help(vault) should succeed");
            let value = result.value.expect("help(vault) should return a value");
            let text = value.as_str().expect("help(vault) should return a string");
            assert!(
                text.contains("vault.note"),
                "help(vault) should describe vault.note, got: {text}"
            );
        }

        #[test]
        fn dataviewjs_help_bare_function_shows_tip() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            // vault.note is a function with no registered help — should get a hint.
            let result =
                evaluate_dataview_js_query(&paths, "help(vault.note)", Some("Dashboard.md"))
                    .expect("help(vault.note) should succeed");
            // vault.note _does_ have registered help, so verify it includes Parameters.
            let value = result.value.expect("should have a value");
            let text = value.as_str().expect("should be string");
            assert!(
                text.contains("Parameters"),
                "vault.note help should include Parameters, got: {text}"
            );
        }

        #[test]
        fn dataviewjs_special_variables_track_last_result_and_error() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let session = DataviewJsSession::new(
                &paths,
                Some("Dashboard.md"),
                DataviewJsEvalOptions::default(),
            )
            .expect("session should open");

            // Evaluate a value, then manually inject _ like the REPL does.
            let r1 = session.evaluate("42").expect("should evaluate");
            let json = serde_json::to_string(&r1.value.as_ref().unwrap()).unwrap();
            let escaped = serde_json::to_string(&json).unwrap();
            session
                .evaluate(&format!("globalThis._ = JSON.parse({escaped});"))
                .expect("injection should succeed");

            let r2 = session.evaluate("_").expect("_ should be defined");
            assert_eq!(
                r2.value,
                Some(Value::Number(serde_json::Number::from(42))),
                "_ should hold last result"
            );
        }

        #[test]
        fn dataviewjs_app_stub_get_name_returns_string() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result =
                evaluate_dataview_js_query(&paths, "app.vault.getName()", Some("Dashboard.md"))
                    .expect("app.vault.getName() should succeed");
            let value = result.value.expect("should have a value");
            let name = value.as_str().expect("should be a string");
            assert!(!name.is_empty(), "vault name should be non-empty");
        }

        #[test]
        fn dataviewjs_app_stub_get_markdown_files_returns_array() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                "app.vault.getMarkdownFiles()",
                Some("Dashboard.md"),
            )
            .expect("app.vault.getMarkdownFiles() should succeed");
            let value = result.value.expect("should have a value");
            let files = value.as_array().expect("should be an array");
            assert!(
                !files.is_empty(),
                "vault should have at least one markdown file"
            );
            let first = &files[0];
            assert!(
                first.get("path").is_some(),
                "each file should have a path field"
            );
        }

        #[test]
        fn dataviewjs_app_stub_workspace_throws_descriptive_error() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let err = evaluate_dataview_js_query(
                &paths,
                "app.workspace.activeLeaf",
                Some("Dashboard.md"),
            )
            .expect_err("app.workspace should throw");
            assert!(
                err.to_string().contains("not supported"),
                "error should explain why workspace is unavailable, got: {err}"
            );
        }

        #[test]
        fn dataviewjs_host_exec_requires_none_sandbox_and_explicit_profile() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let source = r#"host.exec(["must-not-run"])"#;

            let strict_error = evaluate_dataview_js_with_options(
                &paths,
                source,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Strict),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("strict sandbox must reject host execution");
            assert!(strict_error.to_string().contains("requires --sandbox none"));

            let missing_profile_error = evaluate_dataview_js_with_options(
                &paths,
                source,
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: None,
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("missing permission profile must reject host execution");
            assert!(missing_profile_error
                .to_string()
                .contains("requires an explicit profile with execute access"));
        }

        #[test]
        fn granted_host_commands_run_exactly_from_a_sandboxed_tier() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let granted = test_host_exec_argv(&test_host_output_command("alpha"));
            let other = test_host_exec_argv(&test_host_output_command("beta"));
            let run = |argv: &[String], opts: &str| {
                evaluate_dataview_js_with_options(
                    &paths,
                    &format!(
                        "host.exec({}, {opts}).stdout",
                        serde_json::to_string(argv).expect("argv")
                    ),
                    Some("Dashboard.md"),
                    DataviewJsEvalOptions {
                        sandbox: Some(JsRuntimeSandbox::Strict),
                        permission_profile: Some("writer".to_string()),
                        granted_host_commands: vec![granted.clone()],
                        ..DataviewJsEvalOptions::default()
                    },
                )
            };

            let result = run(&granted, "{}").expect("the granted command runs");
            assert_eq!(result.value, Some(serde_json::json!("alpha")));
            let refused = run(&other, "{}").expect_err("other commands stay refused");
            assert!(
                refused.to_string().contains("granted host commands"),
                "{refused}"
            );
            let env = run(&granted, r#"{ env: { PATH: "" } }"#)
                .expect_err("a granted command keeps its environment");
            assert!(env.to_string().contains("environment"), "{env}");
        }

        #[test]
        fn sandboxed_javascript_runs_under_a_profile_without_execute() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");
            let run = |sandbox| {
                evaluate_dataview_js_with_options(
                    &paths,
                    "1 + 1",
                    Some("Dashboard.md"),
                    DataviewJsEvalOptions {
                        sandbox: Some(sandbox),
                        permission_profile: Some("writer".to_string()),
                        ..DataviewJsEvalOptions::default()
                    },
                )
            };

            for sandbox in [
                JsRuntimeSandbox::Strict,
                JsRuntimeSandbox::Fs,
                JsRuntimeSandbox::Net,
            ] {
                let result = run(sandbox).expect("sandboxed code needs no execute grant");
                assert_eq!(result.value, Some(serde_json::json!(2)));
            }
            let error = run(JsRuntimeSandbox::None).expect_err("none needs execute");
            assert!(error.to_string().contains("execute"), "{error}");
        }

        #[test]
        fn dataviewjs_host_exec_and_shell_respect_permissions() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            write_host_permissions_config(&vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let exec_argv =
                serde_json::to_string(&test_host_exec_argv(&test_host_output_command("alpha")))
                    .expect("argv json should serialize");
            let exec_result = evaluate_dataview_js_with_options(
                &paths,
                &format!("host.exec({exec_argv})"),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: Some(HOST_SUCCESS_TIMEOUT),
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: Some("exec_only".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("host.exec should succeed under execute access");
            let exec_value = exec_result.value.expect("host.exec should return a value");
            assert_eq!(exec_value["success"], Value::Bool(true));
            assert_eq!(exec_value["stdout"], Value::String("alpha".to_string()));
            assert_eq!(exec_value["timed_out"], Value::Bool(false));
            assert_eq!(
                exec_value["invocation"]["kind"],
                Value::String("exec".to_string())
            );

            let shell_command =
                serde_json::to_string(&test_host_output_command("alpha")).expect("shell string");
            let shell_error = evaluate_dataview_js_with_options(
                &paths,
                &format!("host.shell({shell_command})"),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: Some(HOST_SUCCESS_TIMEOUT),
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: Some("exec_only".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect_err("shell access should be denied without shell permission");
            assert!(shell_error
                .to_string()
                .contains("does not allow shell access"));

            let shell_result = evaluate_dataview_js_with_options(
                &paths,
                &format!("host.shell({shell_command})"),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: Some(HOST_SUCCESS_TIMEOUT),
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: Some("shell_runner".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("host.shell should succeed with shell access");
            let shell_value = shell_result
                .value
                .expect("host.shell should return a value");
            assert_eq!(shell_value["success"], Value::Bool(true));
            assert_eq!(shell_value["stdout"], Value::String("alpha".to_string()));
            assert_eq!(
                shell_value["invocation"]["kind"],
                Value::String("shell".to_string())
            );
        }

        #[test]
        fn dataviewjs_host_exec_inherits_runtime_timeout_and_truncates_output() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            write_host_permissions_config(&vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let timeout_argv =
                serde_json::to_string(&test_host_exec_argv(&test_host_sleep_command(1_000)))
                    .expect("argv json should serialize");
            let started = Instant::now();
            let timeout_result = evaluate_dataview_js_with_options(
                &paths,
                &format!("host.exec({timeout_argv}, {{ timeout_ms: 5000 }})"),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: Some(Duration::from_millis(75)),
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: Some("exec_only".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("timed-out host.exec should still return a report");
            // The runtime's 75 ms limit wins over the call's own 5 s.
            assert!(started.elapsed() < Duration::from_secs(4));
            let timeout_value = timeout_result
                .value
                .expect("timed-out host.exec should return a value");
            assert_eq!(timeout_value["success"], Value::Bool(false));
            assert_eq!(timeout_value["timed_out"], Value::Bool(true));

            let noisy_argv = serde_json::to_string(&test_host_exec_argv(
                &test_host_output_command(&"a".repeat(256)),
            ))
            .expect("argv json should serialize");
            let noisy_result = evaluate_dataview_js_with_options(
                &paths,
                &format!("host.exec({noisy_argv}, {{ max_output_bytes: 32 }})"),
                Some("Dashboard.md"),
                DataviewJsEvalOptions {
                    timeout: Some(HOST_SUCCESS_TIMEOUT),
                    sandbox: Some(JsRuntimeSandbox::None),
                    permission_profile: Some("exec_only".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("host.exec should capture truncated output");
            let noisy_value = noisy_result
                .value
                .expect("host.exec should return a truncation report");
            assert_eq!(noisy_value["success"], Value::Bool(true));
            assert_eq!(noisy_value["truncated_stdout"], Value::Bool(true));
            assert_eq!(
                noisy_value["stdout"]
                    .as_str()
                    .expect("stdout should be a string")
                    .len(),
                32
            );
        }

        fn write_host_permissions_config(vault_root: &Path) {
            fs::write(
                vault_root.join(".vulcan/config.toml"),
                r#"
[permissions.profiles.exec_only]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.shell_runner]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "allow"
"#,
            )
            .expect("host permission config should be written");
        }

        /// Room for a cold shell start on a loaded CI runner (`PowerShell` can
        /// take over a second); these runs check results, not timing.
        const HOST_SUCCESS_TIMEOUT: Duration = Duration::from_secs(30);

        fn test_host_exec_argv(command: &str) -> Vec<String> {
            #[cfg(target_os = "windows")]
            {
                vec![
                    default_system_shell().display().to_string(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    command.to_string(),
                ]
            }
            #[cfg(not(target_os = "windows"))]
            {
                vec![
                    default_system_shell().display().to_string(),
                    "-lc".to_string(),
                    command.to_string(),
                ]
            }
        }

        fn test_host_output_command(text: &str) -> String {
            #[cfg(target_os = "windows")]
            {
                format!("[Console]::Out.Write('{text}')")
            }
            #[cfg(not(target_os = "windows"))]
            {
                format!("printf %s {text}")
            }
        }

        fn test_host_sleep_command(milliseconds: u64) -> String {
            #[cfg(target_os = "windows")]
            {
                format!("while ($true) {{ Start-Sleep -Milliseconds {milliseconds} }}")
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = milliseconds;
                "while :; do :; done".to_string()
            }
        }

        /// Fields Vulcan adds to Dataview's objects on purpose.
        const PROBE_VULCAN_EXTENSIONS: &[&str] = &[".fileKeys.basename", ".fileKeys.properties"];

        /// Paths where Vulcan's run of the probe still differs from Obsidian's. Remove an
        /// entry once its fix lands: the test fails when a listed path no longer differs.
        const PROBE_KNOWN_DIVERGENCES: &[&str] = &[
            ".pageKeys.Inline",
            ".pageKeys.Inline Date",
            ".pageKeys.Inline Duration",
            ".pageKeys.Inline Link",
            ".pageKeys.Inline Number",
            ".pageKeys.inline date",
            ".pageKeys.inline duration",
            ".pageKeys.inline link",
            ".pageKeys.inline number",
            ".pageValues.Inline",
            ".pageValues.Inline Date",
            ".pageValues.Inline Duration",
            ".pageValues.Inline Link",
            ".pageValues.Inline Number",
            ".pageValues.inline date",
            ".pageValues.inline duration",
            ".pageValues.inline link",
            ".pageValues.inline number",
        ];

        #[test]
        fn dataviewjs_probe_matches_obsidian() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview-probe", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let note = fs::read_to_string(vault_root.join("Dataview Probe.md"))
                .expect("probe note should be readable");
            let source = note
                .split("```dataviewjs\n")
                .nth(1)
                .and_then(|rest| rest.split("\n```").next())
                .expect("probe note should hold a dataviewjs block");
            let result = evaluate_dataview_js_with_options(
                &paths,
                source,
                Some("Dataview Probe.md"),
                DataviewJsEvalOptions {
                    sandbox: Some(JsRuntimeSandbox::Fs),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("probe should evaluate");
            let [DataviewJsOutput::Paragraph { text }] = result.outputs.as_slice() else {
                panic!(
                    "probe should render one paragraph, got {:?}",
                    result.outputs
                );
            };
            let actual: Value = serde_json::from_str(
                text.trim_start_matches("```json\n")
                    .trim_end_matches("\n```"),
            )
            .expect("probe output should be JSON");
            let expected: Value = serde_json::from_str(
                &fs::read_to_string(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../tests/fixtures/dataview-probe-obsidian.json"),
                )
                .expect("Obsidian probe output should be readable"),
            )
            .expect("Obsidian probe output should be JSON");

            let mut differences = Vec::new();
            probe_differences("", &expected, &actual, &mut differences);
            let covered = |path: &str, entry: &str| {
                path.strip_prefix(entry)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
            };
            let allowed = || {
                PROBE_VULCAN_EXTENSIONS
                    .iter()
                    .chain(PROBE_KNOWN_DIVERGENCES)
            };
            let unexpected = differences
                .iter()
                .filter(|(path, _)| !allowed().any(|entry| covered(path, entry)))
                .map(|(path, detail)| format!("{path}: {detail}"))
                .collect::<Vec<_>>();
            let fixed = allowed()
                .filter(|entry| !differences.iter().any(|(path, _)| covered(path, entry)))
                .collect::<Vec<_>>();
            assert!(
                unexpected.is_empty() && fixed.is_empty(),
                "unexpected differences from Obsidian:\n{}\nlisted paths that now match: {fixed:?}",
                unexpected.join("\n")
            );
        }

        /// Collects the leaf paths where `actual` differs from `expected`. Objects compare key by
        /// key and same-length arrays of objects or arrays element by element.
        fn probe_differences(
            path: &str,
            expected: &Value,
            actual: &Value,
            out: &mut Vec<(String, String)>,
        ) {
            match (expected, actual) {
                (Value::Object(expected), Value::Object(actual)) => {
                    let keys = expected
                        .keys()
                        .chain(actual.keys())
                        .collect::<std::collections::BTreeSet<_>>();
                    for key in keys {
                        let child = format!("{path}.{key}");
                        match (expected.get(key), actual.get(key)) {
                            (Some(expected), Some(actual)) => {
                                probe_differences(&child, expected, actual, out);
                            }
                            (expected, actual) => out
                                .push((child, format!("obsidian={expected:?} vulcan={actual:?}"))),
                        }
                    }
                }
                (Value::Array(expected), Value::Array(actual))
                    if expected.len() == actual.len()
                        && expected
                            .iter()
                            .any(|value| value.is_object() || value.is_array()) =>
                {
                    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                        probe_differences(&format!("{path}[{index}]"), expected, actual, out);
                    }
                }
                _ if expected != actual => out.push((
                    path.to_string(),
                    format!("obsidian={expected} vulcan={actual}"),
                )),
                _ => {}
            }
        }

        fn copy_fixture_vault(name: &str, destination: &Path) {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/vaults")
                .join(name);

            copy_dir_recursive(&source, destination);
            fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
        }

        fn copy_dir_recursive(source: &Path, destination: &Path) {
            fs::create_dir_all(destination).expect("destination directory should be created");

            for entry in fs::read_dir(source).expect("source directory should be readable") {
                let entry = entry.expect("directory entry should be readable");
                let file_type = entry.file_type().expect("file type should be readable");
                if entry.file_name() == ".vulcan" {
                    continue;
                }
                let target = destination.join(entry.file_name());

                if file_type.is_dir() {
                    copy_dir_recursive(&entry.path(), &target);
                } else if file_type.is_file() {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).expect("parent directory should exist");
                    }
                    fs::copy(entry.path(), target).expect("file should be copied");
                }
            }
        }
    }
}

#[cfg(feature = "js_runtime")]
pub use runtime::{
    evaluate_dataview_js, evaluate_dataview_js_query, evaluate_dataview_js_with_options,
    DataviewJsSession,
};
