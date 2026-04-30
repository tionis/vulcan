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

#[derive(Clone, Default)]
pub struct DataviewJsEvalOptions {
    pub timeout: Option<Duration>,
    pub sandbox: Option<JsRuntimeSandbox>,
    pub permission_profile: Option<String>,
    pub resolved_permissions: Option<ResolvedPermissionProfile>,
    pub deterministic_static: bool,
    pub disable_policy_hooks: bool,
    pub tool_registry: Option<Arc<dyn DataviewJsToolRegistry>>,
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
            .field("disable_policy_hooks", &self.disable_policy_hooks)
            .field(
                "tool_registry",
                &self.tool_registry.as_ref().map(|_| "<registered>"),
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
    use rquickjs::function::Func;
    use rquickjs::{
        CatchResultExt, CaughtError, Context, Ctx, Exception, Runtime, Value as JsValue,
    };
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    use super::{
        DataviewJsError, DataviewJsEvalOptions, DataviewJsOutput, DataviewJsResult,
        DataviewJsToolDescriptor, DataviewJsToolRegistry,
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
    use crate::periodic::{
        list_daily_note_events, list_events_between, load_events_for_periodic_note,
        resolve_periodic_note, today_utc_string,
    };
    use crate::permissions::{resolve_permission_profile, PermissionGuard, ProfilePermissionGuard};
    use crate::properties::{load_note_index, NoteRecord};
    use crate::refactor::{
        merge_tags, rename_alias, rename_block_ref, rename_heading, rename_property,
        set_note_property,
    };
    use crate::resolve_note_reference;
    use crate::search::{search_vault_with_filter, SearchQuery};
    use crate::web::{
        fetch_web, prepare_search_backend, search_web, WebFetchReport, WebSearchReport,
    };
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
        note_index: Mutex<HashMap<String, NoteRecord>>,
        periodic_config: crate::PeriodicConfig,
        inbox_config: crate::InboxConfig,
        web_config: WebConfig,
        shell_path: Option<PathBuf>,
        sandbox: JsRuntimeSandbox,
        permissions: Option<ProfilePermissionGuard>,
        deterministic_static: bool,
        runtime_timeout: Option<Duration>,
        transaction: Mutex<Option<JsTransactionState>>,
        tool_registry: Option<Arc<dyn DataviewJsToolRegistry>>,
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

    const DATAVIEW_JS_PRELUDE: &str = r#"
const __vulcanDeterministicStatic = Boolean(globalThis.__vulcan_deterministic_static);

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

  array() {
    return Array.from(this.values);
  }

  where(predicate) {
    return new DataArray(this.values.filter((value, index) => predicate(value, index)));
  }

  filter(predicate) {
    return this.where(predicate);
  }

  map(mapper) {
    return new DataArray(this.values.map((value, index) => mapper(value, index)));
  }

  flatMap(mapper) {
    return new DataArray(this.values.flatMap((value, index) => __vulcanAsArray(mapper(value, index))));
  }

  sort(key, dir = "asc") {
    const values = this.array();
    values.sort((left, right) => __vulcanCompareForSort(__vulcanResolveKey(left, key), __vulcanResolveKey(right, key), dir));
    return new DataArray(values);
  }

  sortInPlace(key, dir = "asc") {
    this.values = this.sort(key, dir).values;
    return this;
  }

  sortBy(key, dir = "asc") {
    return this.sort(key, dir);
  }

  groupBy(key) {
    const order = [];
    const groups = new Map();
    for (const value of this.values) {
      const groupKey = __vulcanResolveKey(value, key);
      const serialized = JSON.stringify(__vulcanPlain(groupKey));
      if (!groups.has(serialized)) {
        groups.set(serialized, { key: groupKey, rows: [] });
        order.push(serialized);
      }
      groups.get(serialized).rows.push(value);
    }
    return new DataArray(order.map((serialized) => {
      const group = groups.get(serialized);
      return { key: group.key, rows: new DataArray(group.rows) };
    }));
  }

  groupIn(key) {
    return this.groupBy(key);
  }

  unique() {
    const seen = new Set();
    const values = [];
    for (const value of this.values) {
      const serialized = JSON.stringify(__vulcanPlain(value));
      if (!seen.has(serialized)) {
        seen.add(serialized);
        values.push(value);
      }
    }
    return new DataArray(values);
  }

  distinct() {
    return this.unique();
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

  indexOf(value) {
    const serialized = JSON.stringify(__vulcanPlain(value));
    return this.values.findIndex((candidate) => JSON.stringify(__vulcanPlain(candidate)) === serialized);
  }

  find(predicate) {
    return this.values.find((value, index) => predicate(value, index));
  }

  findIndex(predicate) {
    return this.values.findIndex((value, index) => predicate(value, index));
  }

  includes(value) {
    return this.indexOf(value) >= 0;
  }

  join(separator = ", ") {
    return this.values.map(__vulcanRenderScalar).join(separator);
  }

  every(predicate) {
    return this.values.every((value, index) => predicate(value, index));
  }

  some(predicate) {
    return this.values.some((value, index) => predicate(value, index));
  }

  none(predicate) {
    return !this.some(predicate);
  }

  forEach(callback) {
    this.values.forEach((value, index) => callback(value, index));
  }

  mutate(mutator) {
    this.values.forEach((value, index) => mutator(value, index));
    return this;
  }

  into(key) {
    return new DataArray(this.values.map((value) => value == null ? undefined : value[key]));
  }

  expand(expander) {
    const expanded = [];
    const visit = (value) => {
      expanded.push(value);
      const next = expander(value);
      for (const child of __vulcanAsArray(next)) {
        if (child != null) {
          visit(child);
        }
      }
    };
    for (const value of this.values) {
      visit(value);
    }
    return new DataArray(expanded);
  }

  forEach(callback) {
    this.values.forEach((value, index) => callback(value, index));
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

function __vulcanResolveKey(value, key) {
  if (key == null) {
    return value;
  }
  if (typeof key === "function") {
    return key(value);
  }
  return value == null ? undefined : value[key];
}

function __vulcanNormalizeComparable(value) {
  if (value instanceof Date) {
    return value.toISOString();
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

function __vulcanSerialize(value) {
  return JSON.stringify(__vulcanPlain(value));
}

function __vulcanSerializeComparable(value) {
  const normalized = __vulcanNormalizeComparable(value);
  return JSON.stringify(normalized === undefined ? null : normalized);
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
  const revived = { ...file };
  for (const key of ["day", "mday", "cday"]) {
    if (typeof revived[key] === "string") {
      const millis = __vulcan_date_millis(revived[key]);
      if (millis != null) {
        revived[key] = new VulcanDateTime(new Date(millis));
      }
    }
  }
  return revived;
}

function __vulcanRevivePage(page) {
  if (!page || typeof page !== "object" || Array.isArray(page)) {
    return page;
  }
  return {
    ...page,
    file: __vulcanReviveFileMetadata(page.file),
  };
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

  static fromISO(value) {
    const millis = __vulcan_date_millis(String(value));
    return millis == null ? null : new VulcanDateTime(new Date(millis));
  }

  static fromMillis(value) {
    return new VulcanDateTime(new Date(Number(value)));
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

  toISO() {
    return this._date.toISOString();
  }

  toMillis() {
    return this._date.getTime();
  }

  toJSDate() {
    return new Date(this._date.getTime());
  }

  toFormat(format) {
    return __vulcan_format_date(this.toMillis(), String(format));
  }

  plus(value) {
    const millis = __vulcanDurationMillis(value);
    return new VulcanDateTime(new Date(this.toMillis() + (millis ?? 0)));
  }

  minus(value) {
    const millis = __vulcanDurationMillis(value);
    return new VulcanDateTime(new Date(this.toMillis() - (millis ?? 0)));
  }

  startOf(unit) {
    const date = this.toJSDate();
    const normalized = String(unit).toLowerCase();
    if (normalized === "day") {
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "month") {
      date.setUTCDate(1);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "year") {
      date.setUTCMonth(0, 1);
      date.setUTCHours(0, 0, 0, 0);
    } else if (normalized === "week") {
      const weekday = (date.getUTCDay() + 6) % 7;
      date.setUTCDate(date.getUTCDate() - weekday);
      date.setUTCHours(0, 0, 0, 0);
    }
    return new VulcanDateTime(date);
  }

  endOf(unit) {
    const normalized = String(unit).toLowerCase();
    const start = this.startOf(unit);
    if (normalized === "day") {
      return start.plus(VulcanDuration.fromObject({ days: 1 })).minus(1);
    }
    if (normalized === "month") {
      const date = start.toJSDate();
      date.setUTCMonth(date.getUTCMonth() + 1);
      return new VulcanDateTime(date).minus(1);
    }
    if (normalized === "year") {
      const date = start.toJSDate();
      date.setUTCFullYear(date.getUTCFullYear() + 1);
      return new VulcanDateTime(date).minus(1);
    }
    if (normalized === "week") {
      return start.plus(VulcanDuration.fromObject({ weeks: 1 })).minus(1);
    }
    return start;
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

  get year() {
    return this._date.getUTCFullYear();
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
}

class VulcanDuration {
  constructor(millis, text) {
    this.__vulcanDuration = true;
    this.millis = Number(millis) || 0;
    this.text = text ?? `${this.millis}ms`;
  }

  static fromMillis(value) {
    return new VulcanDuration(Number(value) || 0, `${Number(value) || 0}ms`);
  }

  static fromISO(value) {
    const millis = __vulcan_duration_millis(String(value));
    return millis == null ? null : new VulcanDuration(millis, String(value));
  }

  static fromObject(value = {}) {
    const millis =
      Number(value.millisecond ?? value.milliseconds ?? 0) +
      Number(value.second ?? value.seconds ?? 0) * 1000 +
      Number(value.minute ?? value.minutes ?? 0) * 60 * 1000 +
      Number(value.hour ?? value.hours ?? 0) * 60 * 60 * 1000 +
      Number(value.day ?? value.days ?? 0) * 24 * 60 * 60 * 1000 +
      Number(value.week ?? value.weeks ?? 0) * 7 * 24 * 60 * 60 * 1000 +
      Number(value.month ?? value.months ?? 0) * 30 * 24 * 60 * 60 * 1000 +
      Number(value.year ?? value.years ?? 0) * 365 * 24 * 60 * 60 * 1000;
    return new VulcanDuration(millis);
  }

  toMillis() {
    return this.millis;
  }

  toFormat(format) {
    return __vulcan_format_duration(this.millis, String(format));
  }

  plus(value) {
    const millis = __vulcanDurationMillis(value);
    return new VulcanDuration(this.millis + (millis ?? 0));
  }

  minus(value) {
    const millis = __vulcanDurationMillis(value);
    return new VulcanDuration(this.millis - (millis ?? 0));
  }

  toString() {
    return this.text;
  }
}

function __vulcanRenderQueryMarkdown(result) {
  if (!result) {
    return "";
  }
  if (result.query_type === "table") {
    const headers = result.columns ?? [];
    const lines = [];
    if (headers.length > 0) {
      lines.push(`| ${headers.join(" | ")} |`);
      lines.push(`| ${headers.map(() => "---").join(" | ")} |`);
    }
    for (const row of result.rows ?? []) {
      lines.push(`| ${headers.map((header) => __vulcanMarkdownEscape(__vulcanRenderScalar(row[header]))).join(" | ")} |`);
    }
    return lines.join("\n");
  }
  if (result.query_type === "list") {
    const columns = result.columns ?? [];
    return (result.rows ?? []).map((row) => {
      if (columns.length === 1) {
        return `- ${__vulcanRenderScalar(row[columns[0]])}`;
      }
      if (columns.length >= 2) {
        return `- ${__vulcanRenderScalar(row[columns[0]])}: ${__vulcanRenderScalar(row[columns[1]])}`;
      }
      return `- ${JSON.stringify(row)}`;
    }).join("\n");
  }
  if (result.query_type === "task") {
    const fileColumn = (result.columns ?? [])[0] ?? "File";
    const lines = [];
    let currentFile = null;
    for (const row of result.rows ?? []) {
      const file = row[fileColumn];
      if (currentFile !== file) {
        currentFile = file;
        lines.push(String(file));
      }
      lines.push(`- [${row.status ?? " "}] ${row.text ?? ""}`);
    }
    return lines.join("\n");
  }
  if (result.query_type === "calendar") {
    const lines = [];
    let currentDate = null;
    const fileColumn = (result.columns ?? [])[1] ?? "File";
    for (const row of result.rows ?? []) {
      if (currentDate !== row.date) {
        currentDate = row.date;
        lines.push(String(row.date));
      }
      lines.push(`- ${__vulcanRenderScalar(row[fileColumn])}`);
    }
    return lines.join("\n");
  }
  return JSON.stringify(result);
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
    link(path, display) {
      if (path == null) {
        return null;
      }
      return display == null
        ? `[[${String(path)}]]`
        : `[[${String(path)}|${String(display)}]]`;
    },
    embed(value, shouldEmbed = true) {
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
    return new DataArray(JSON.parse(__vulcan_pages_json(source)).map(__vulcanRevivePage));
  },
  page(path) {
    return __vulcanRevivePage(JSON.parse(__vulcan_page_json(path)));
  },
  current() {
    return __vulcanRevivePage(JSON.parse(__vulcan_current_json()));
  },
  query(dql, file, _settings) {
    return JSON.parse(__vulcan_query_json(dql, file));
  },
  tryQuery(dql, file, settings) {
    const result = dv.query(dql, file, settings);
    if (!result.successful) {
      throw new Error(result.error ?? "Dataview query failed");
    }
    return result.value;
  },
  queryMarkdown(dql, file, settings) {
    return __vulcanRenderQueryMarkdown(dv.tryQuery(dql, file, settings));
  },
  tryQueryMarkdown(dql, file, settings) {
    return dv.queryMarkdown(dql, file, settings);
  },
  execute(dql) {
    const result = dv.tryQuery(dql);
    __vulcan_emit(JSON.stringify({ kind: "query", result }));
    return result;
  },
  table(headers, rows) {
    const normalizedRows = __vulcanAsArray(rows).map((row) => __vulcanAsArray(row).map(__vulcanPlain));
    __vulcan_emit(JSON.stringify({ kind: "table", headers: __vulcanAsArray(headers).map(String), rows: normalizedRows }));
  },
  list(items) {
    __vulcan_emit(JSON.stringify({ kind: "list", items: __vulcanAsArray(items).map(__vulcanPlain) }));
  },
  taskList(tasks, groupByFile = false) {
    __vulcan_emit(JSON.stringify({ kind: "task_list", tasks: __vulcanAsArray(tasks).map(__vulcanPlain), group_by_file: !!groupByFile }));
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
    load(path) {
      return __vulcan_load(path);
    },
    csv(path, originFile) {
      return new DataArray(JSON.parse(__vulcan_csv_json(path, originFile)));
    },
    normalize(path, originFile) {
      return __vulcan_normalize(path, originFile);
    },
  },
  view(path, input) {
    const source = __vulcan_load_view(path);
    const previousInput = globalThis.input;
    try {
      globalThis.input = input ?? null;
      return __vulcanPrivateEval(source);
    } finally {
      globalThis.input = previousInput;
    }
  },
  date(input) {
    const millis = __vulcan_date_millis(
      input instanceof Date ? input.toISOString() : String(input)
    );
    return millis == null ? null : new VulcanDateTime(new Date(millis));
  },
  duration(input) {
    const millis = __vulcan_duration_millis(String(input));
    return {
      __vulcanDuration: true,
      millis,
      text: String(input),
      toString() {
        return this.text;
      },
    };
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
    return JSON.parse(__vulcanSerialize(value));
  },
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

const vault = {
  note(path) {
    return __vulcanNote(JSON.parse(__vulcan_page_json(path)));
  },
  notes(source) {
    return __vulcanNotes(JSON.parse(__vulcan_pages_json(source)));
  },
  query(dql, opts = {}) {
    return dv.tryQuery(dql, opts?.file ?? null, opts);
  },
  search(query, opts = {}) {
    return JSON.parse(__vulcan_search_json(String(query), opts?.limit ?? null));
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
    return __vulcanNote(
      __vulcanMutation("create", {
        path,
        content: opts?.content ?? "",
        frontmatter: opts?.frontmatter ?? null,
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

const console = {
  log(...args) {
    __vulcan_emit(JSON.stringify({
      kind: "paragraph",
      text: args.map((value) => __vulcanRenderScalar(value)).join(" ")
    }));
  },
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
      "  dv       — Dataview-compatible query API (dv.pages, dv.table, etc.)",
      "  tools    — registry-backed custom tools (tools.list, tools.get, tools.call)",
      "  host     — permission-gated host process execution (host.exec, host.shell)",
      "  web      — external web search and fetch (requires --sandbox net)",
      "  console  — console.log() for output",
      "  app      — Obsidian API compatibility stub (app.vault.*)",
      "  _        — last successful result",
      "  _error   — last error message",
      "",
      "Usage: help(vault), help(dv), help(vault.search), etc.",
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
  `vault.query(dql: string, opts?: { file?: string }): QueryResult

Run one DQL query against the indexed vault.

Parameters:
  dql  - Query DSL string.
  opts - Optional execution settings such as the current file context.

Example:
  vault.query('TABLE status FROM "Projects"')

See also: vault.notes(), vault.search()`
);
__vulcanRegisterHelp(
  vault.search,
  `vault.search(query: string, opts?: { limit?: number }): SearchReport

Run indexed full-text search.

Parameters:
  query - Search text or phrase.
  opts  - Optional result controls such as limit.

Example:
  vault.search("Alpha", { limit: 3 })

See also: vault.notes(), vault.query()`
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
  `vault.create(path: string, opts?: { content?: string, frontmatter?: object }): Note

Create a new note and return it as a Note object. Requires --sandbox fs or higher.

Parameters:
  path - Target note path.
  opts - Optional content and frontmatter fields for the new note.

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

List visible vault-native custom tools with metadata such as description, sandbox, packs, and
callability.

Example:
  tools.list().map((tool) => tool.name)

See also: tools.get(), tools.call()`
);
__vulcanRegisterHelp(
  tools.get,
  `tools.get(name: string): CustomTool

Read one custom tool definition, including static metadata and the Markdown documentation body.

Parameters:
  name - Tool name, directory name, or manifest path.

Example:
  tools.get("summarize_meeting").body

See also: tools.list(), tools.call()`
);
__vulcanRegisterHelp(
  tools.call,
  `tools.call(name: string, input?: object, opts?: object): unknown

Invoke one custom tool with validated JSON input.

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
  vault,
  `vault — Vulcan vault API

Available methods and namespaces:
  vault.note(path)       — resolve one note
  vault.notes(source?)   — query a note collection (DataArray)
  vault.query(dql)       — run a DQL query string
  vault.search(q, opts?) — full-text search
  vault.graph.*          — link graph traversal (shortestPath, hubs, deadEnds, etc.)
  vault.daily.*          — periodic/daily note helpers (today, get, range, append)
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
  dv.execute(dql)          — run a DQL query and emit its output
  dv.io.csv(path)          — load a CSV file as DataArray
  dv.io.load(path)         — load a file as raw text
  dv.func.*                — DataviewJS utility functions (date, duration, etc.)
  dv.luxon.*               — Luxon DateTime and Duration classes

Use dv.pages('#tag') or dv.pages('"Folder"') to filter.`
);

__vulcanRegisterHelp(
  tools,
  `tools — Vault-native custom tool registry

  tools.list()             — list visible custom tools
  tools.get(name)          — read one tool manifest plus documentation body
  tools.call(name, input?) — invoke one custom tool with validated JSON input

Use help(tools.call) for execution details and nested-call limits.`
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
        stat: { mtime: page.file?.mtime ?? 0, ctime: 0, size: 0 },
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
globalThis.host = host;
globalThis.web = web;
globalThis.console = console;
globalThis.help = help;
globalThis.app = app;
globalThis._ = undefined;
globalThis._error = undefined;
globalThis.this = dv.current();
globalThis.eval = undefined;
globalThis.Function = undefined;
"#;

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
        #[allow(clippy::needless_pass_by_value)]
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
            if let Some(permissions) = permissions.as_ref() {
                permissions
                    .check_execute()
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            }
            let mut note_index = load_note_index(paths)
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            if let Some(permissions) = permissions.as_ref() {
                let read_filter = permissions.read_filter();
                note_index.retain(|_, note| read_filter.is_allowed(&note.document_path));
            }
            let sandbox = options
                .sandbox
                .unwrap_or(loaded_config.js_runtime.default_sandbox);
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
                note_index: Mutex::new(note_index),
                periodic_config: loaded_config.periodic.clone(),
                inbox_config: loaded_config.inbox.clone(),
                web_config: loaded_config.web.clone(),
                shell_path: loaded_config.templates.shell_path.clone(),
                sandbox,
                permissions,
                deterministic_static: options.deterministic_static,
                runtime_timeout: timeout,
                transaction: Mutex::new(None),
                tool_registry: options.tool_registry,
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
            context.with(|ctx| -> Result<(), DataviewJsError> {
                install_dataview_globals(ctx.clone(), Arc::clone(&state), Arc::clone(&outputs))?;
                ctx.eval::<(), _>(DATAVIEW_JS_PRELUDE)
                    .catch(&ctx)
                    .map_err(|error| map_caught_runtime_error(&error, &timeout_description))?;
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
                .with(|ctx| evaluate_value(&ctx, source, &timeout_description));
            let drain_result = drain_pending_jobs(&self.runtime, &timeout_description);
            self.runtime.set_interrupt_handler(None);
            drain_result?;
            let value = eval_result?;
            let outputs = self.outputs.lock().map_err(|_| {
                DataviewJsError::Message("DataviewJS output lock poisoned".to_string())
            })?;

            Ok(DataviewJsResult {
                outputs: outputs.clone(),
                value,
            })
        }
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
                    with_note_index(&pages_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            load_pages_from_source(
                                &pages_state.paths,
                                note_index,
                                pages_state.current_file.as_deref(),
                                source.as_deref(),
                            )
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let single_page_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_page_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    with_note_index(&single_page_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            page_object_by_reference(
                                &single_page_state.paths,
                                note_index,
                                &path,
                            )
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let current_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_current_json",
                Func::from(move |ctx: Ctx<'_>| {
                    with_note_index(&current_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            current_state
                                .current_file
                                .as_deref()
                                .map(|path| {
                                    page_object_by_reference(&current_state.paths, note_index, path)
                                })
                                .transpose()
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?
                                .unwrap_or(Value::Null),
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_details_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_details_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    with_note_index(&note_details_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            load_note_details(&note_details_state.paths, note_index, &path)
                                .map_err(|error| {
                                    Exception::throw_message(&ctx, &error.to_string())
                                })?,
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_read_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_read_json",
                Func::from(move |ctx: Ctx<'_>, path: String, options_json: String| {
                    let options = parse_json_string::<crate::NoteReadOptions>(&ctx, &options_json)?;
                    with_note_index(&note_read_state, &ctx, |_note_index| {
                        to_json_string(
                            &ctx,
                            read_note_selection(&note_read_state.paths, &path, &options).map_err(
                                |error| Exception::throw_message(&ctx, &error.to_string()),
                            )?,
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_links_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_links_json",
                Func::from(move |ctx: Ctx<'_>, path: String, direction: String| {
                    with_note_index(&note_links_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            load_note_relationships(
                                &note_links_state.paths,
                                note_index,
                                &path,
                                &direction,
                                note_links_state.permissions.as_ref(),
                            )
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    })
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let note_neighbors_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_note_neighbors_json",
                Func::from(move |ctx: Ctx<'_>, path: String, depth: i32| {
                    with_note_index(&note_neighbors_state, &ctx, |note_index| {
                        to_json_string(
                            &ctx,
                            load_note_neighbors(
                                &note_neighbors_state.paths,
                                note_index,
                                &path,
                                depth,
                                note_neighbors_state.permissions.as_ref(),
                            )
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    })
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
                    with_note_index(&vault_daily_state, &ctx, |note_index| {
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
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
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
                    with_note_index(&vault_daily_range_state, &ctx, |note_index| {
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
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                        )
                    })
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
                        load_state.paths.vault_root(),
                        &path,
                        load_state.current_file.as_deref(),
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
                                csv_state.paths.vault_root(),
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
                        view_state.paths.vault_root(),
                        &path,
                        view_state.current_file.as_deref(),
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
                            "custom tool registry is not available in this runtime",
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
                                "custom tool registry is not available in this runtime",
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

    fn with_note_index<T>(
        state: &JsEvalState,
        ctx: &Ctx<'_>,
        f: impl FnOnce(&HashMap<String, NoteRecord>) -> rquickjs::Result<T>,
    ) -> rquickjs::Result<T> {
        let note_index = state
            .note_index
            .lock()
            .map_err(|_| Exception::throw_message(ctx, "DataviewJS note index lock poisoned"))?;
        f(&note_index)
    }

    fn reload_note_index(state: &JsEvalState) -> Result<(), DataviewJsError> {
        let note_index = load_note_index(&state.paths)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let mut current = state.note_index.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS note index lock poisoned".to_string())
        })?;
        *current = note_index;
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
        paths: &VaultPaths,
        note_index: &HashMap<String, NoteRecord>,
        note: &str,
    ) -> Result<Value, DataviewJsError> {
        let path = resolve_existing_note_path(paths, note)?;
        let source = fs::read_to_string(paths.vault_root().join(&path))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let note = note_by_path(note_index, &path)
            .ok_or_else(|| DataviewJsError::Message(format!("note is not indexed: {path}")))?;
        let config = load_vault_config(paths).config;
        let parsed = parse_document(&source, &config);
        let outline = crate::outline_note(&source, &parsed);
        Ok(serde_json::json!({
            "page": page_object(note),
            "file": FileMetadataResolver::object(note),
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
        paths: &VaultPaths,
        note: &str,
        options: &crate::NoteReadOptions,
    ) -> Result<crate::NoteReadSelection, DataviewJsError> {
        let path = resolve_existing_note_path(paths, note)?;
        let source = fs::read_to_string(paths.vault_root().join(&path))
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
                            .map_or(Value::Null, page_object),
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
                            .map_or(Value::Null, page_object),
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
                    neighbors.push(page_object(related_note));
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
        let mut transaction = state.transaction.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS transaction lock poisoned".to_string())
        })?;
        if transaction.is_none() {
            return Ok(());
        }
        *transaction = None;
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

        for (path, original) in transaction.originals {
            let absolute = state.paths.vault_root().join(&path);
            match original {
                Some(contents) => {
                    if let Some(parent) = absolute.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                    }
                    fs::write(&absolute, contents)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                }
                None => {
                    if absolute.is_file() {
                        fs::remove_file(&absolute)
                            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                    }
                }
            }
        }

        scan_and_reload_state(state)
    }

    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    fn apply_js_mutation(
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
                fs::write(state.paths.vault_root().join(&resolved_path), updated)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                fs::write(&absolute, rendered)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                fs::write(&absolute, updated)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                fs::write(&absolute, updated)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                let existing = fs::read_to_string(&absolute).unwrap_or_default();
                let entry = render_inbox_entry(
                    &state.inbox_config.format,
                    payload_string(&payload, "text")?,
                    &current_utc_timestamp_string(),
                    &today_utc_string(),
                );
                let rendered_entry = if state.inbox_config.timestamp {
                    format!("{} {}", current_utc_timestamp_string(), entry)
                } else {
                    entry
                };
                let updated = state.inbox_config.heading.as_deref().map_or_else(
                    || append_at_end(&existing, &rendered_entry),
                    |heading| append_under_heading(&existing, heading, &rendered_entry),
                );
                fs::write(&absolute, updated)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                let existing = fs::read_to_string(&absolute).unwrap_or_default();
                let text = payload_string(&payload, "text")?;
                let updated = payload.get("heading").and_then(Value::as_str).map_or_else(
                    || append_at_end(&existing, text),
                    |heading| append_under_heading(&existing, heading, text),
                );
                fs::write(&absolute, updated)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
                let report = rename_alias(&state.paths, note, old_alias, new_alias, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
            "refactor.renameHeading" => {
                ensure_no_transaction(state, "vault.refactor.renameHeading()")?;
                let note = payload_string(&payload, "note")?;
                let resolved_note = resolve_existing_note_path(&state.paths, note)?;
                ensure_refactor_access(state, &resolved_note, "vault.refactor.renameHeading()")?;
                let old_heading = payload_string(&payload, "oldHeading")?;
                let new_heading = payload_string(&payload, "newHeading")?;
                let report = rename_heading(&state.paths, note, old_heading, new_heading, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
            "refactor.renameBlockRef" => {
                ensure_no_transaction(state, "vault.refactor.renameBlockRef()")?;
                let note = payload_string(&payload, "note")?;
                let resolved_note = resolve_existing_note_path(&state.paths, note)?;
                ensure_refactor_access(state, &resolved_note, "vault.refactor.renameBlockRef()")?;
                let old_block_id = payload_string(&payload, "oldBlockId")?;
                let new_block_id = payload_string(&payload, "newBlockId")?;
                let report =
                    rename_block_ref(&state.paths, note, old_block_id, new_block_id, false)
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
            "refactor.renameProperty" => {
                ensure_no_transaction(state, "vault.refactor.renameProperty()")?;
                ensure_unrestricted_refactor_scope(state, "vault.refactor.renameProperty()")?;
                let old_key = payload_string(&payload, "oldKey")?;
                let new_key = payload_string(&payload, "newKey")?;
                let report = rename_property(&state.paths, old_key, new_key, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
            "refactor.mergeTags" => {
                ensure_no_transaction(state, "vault.refactor.mergeTags()")?;
                ensure_unrestricted_refactor_scope(state, "vault.refactor.mergeTags()")?;
                let from_tag = payload_string(&payload, "fromTag")?;
                let to_tag = payload_string(&payload, "toTag")?;
                let report = merge_tags(&state.paths, from_tag, to_tag, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
                serde_json::to_value(report)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
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
                let report = move_note(&state.paths, source, &destination, false)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                reload_note_index(state)?;
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
        Ok(())
    }

    fn current_today(state: &JsEvalState, operation: &str) -> Result<String, DataviewJsError> {
        if state.deterministic_static {
            return Err(static_mode_time_denied(operation));
        }
        Ok(today_utc_string())
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
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_execute()
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
        Ok(())
    }

    fn ensure_shell_access(state: &JsEvalState, operation: &str) -> Result<(), DataviewJsError> {
        ensure_static_mode_allows(state, operation, "shell execution")?;
        if let Some(permissions) = state.permissions.as_ref() {
            permissions
                .check_shell()
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            permissions
                .check_execute()
                .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        }
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
        ensure_execute_access(state, "host.exec()")?;
        let argv = parse_host_argv(argv)?;
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
        let note_index = state.note_index.lock().map_err(|_| {
            DataviewJsError::Message("DataviewJS note index lock poisoned".to_string())
        })?;
        let note = note_by_path(&note_index, path)
            .ok_or_else(|| DataviewJsError::Message(format!("note is not indexed: {path}")))?;
        Ok(serde_json::json!({
            "note": page_object(note),
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
            let yaml_source = serde_yaml::to_string(frontmatter)
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
        let yaml = serde_yaml::to_string(value)
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

    fn current_utc_timestamp_string() -> String {
        let seconds = crate::current_utc_timestamp_ms().div_euclid(1_000);
        let days_since_epoch = seconds.div_euclid(86_400);
        let seconds_of_day = seconds.rem_euclid(86_400);
        let hour = seconds_of_day / 3_600;
        let minute = (seconds_of_day % 3_600) / 60;
        let second = seconds_of_day % 60;
        let (year, month, day) = civil_from_days(days_since_epoch);
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }

    fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
        let z = days_since_epoch + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = mp + if mp < 10 { 3 } else { -9 };
        let year = y + i64::from(month <= 2);
        (year, month, day)
    }

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

    fn load_pages_from_source(
        paths: &VaultPaths,
        note_index: &HashMap<String, NoteRecord>,
        current_file: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<Value>, DataviewJsError> {
        let mut notes = if let Some(source) = source.filter(|value| !value.trim().is_empty()) {
            let result = evaluate_dql(
                paths,
                &format!("TABLE WITHOUT ID file.path AS path FROM {source}"),
                current_file,
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
            result
                .rows
                .iter()
                .filter_map(|row| row.get("path").and_then(Value::as_str))
                .filter_map(|path| note_by_path(note_index, path))
                .map(page_object)
                .collect::<Vec<_>>()
        } else {
            let mut notes = note_index.values().cloned().collect::<Vec<_>>();
            notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));
            notes
                .into_iter()
                .map(|note| page_object(&note))
                .collect::<Vec<_>>()
        };
        notes.sort_by(|left, right| {
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
        Ok(notes)
    }

    fn page_object_by_reference(
        paths: &VaultPaths,
        note_index: &std::collections::HashMap<String, NoteRecord>,
        file: &str,
    ) -> Result<Value, DataviewJsError> {
        let resolved_path = resolve_note_reference(paths, file)
            .map(|resolved| resolved.path)
            .or_else(|_| normalize_note_path_for_write(paths, file))?;
        Ok(note_by_path(note_index, &resolved_path).map_or(Value::Null, page_object))
    }

    fn note_by_path<'a>(
        note_index: &'a std::collections::HashMap<String, NoteRecord>,
        path: &str,
    ) -> Option<&'a NoteRecord> {
        note_index.values().find(|note| note.document_path == path)
    }

    fn page_object(note: &NoteRecord) -> Value {
        let mut fields = note
            .properties
            .as_object()
            .cloned()
            .unwrap_or_else(Map::new);
        fields.insert("file".to_string(), FileMetadataResolver::object(note));
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
        Ok(decorate_daily_page_object(note, &normalized_date, events))
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
                note_by_path(note_index, &note.path)
                    .map(|record| decorate_daily_page_object(record, &note.date, note.events))
            })
            .collect())
    }

    fn decorate_daily_page_object(
        note: &NoteRecord,
        date: &str,
        events: Vec<crate::PeriodicEvent>,
    ) -> Value {
        let mut page = page_object(note);
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
            Some(today_utc_string())
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
                Ok(today_utc_string())
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
        vault_root: &Path,
        path: &str,
        origin_file: Option<&str>,
    ) -> Result<String, DataviewJsError> {
        let normalized = normalize_vault_path(vault_root, path, origin_file, true)?;
        fs::read_to_string(vault_root.join(normalized))
            .map_err(|error| DataviewJsError::Message(error.to_string()))
    }

    fn read_csv_file(
        vault_root: &Path,
        path: &str,
        origin_file: Option<&str>,
    ) -> Result<Vec<Map<String, Value>>, DataviewJsError> {
        let normalized = normalize_vault_path(vault_root, path, origin_file, false)?;
        let mut reader = ReaderBuilder::new()
            .flexible(true)
            .from_path(vault_root.join(normalized))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
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
        timeout_description: &str,
    ) -> Result<Option<Value>, DataviewJsError> {
        let value: JsValue<'js> = ctx
            .eval(source)
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
        fn dataviewjs_supports_csv_loading_and_external_views() {
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
                const rows = dv.io.csv("data/owners.csv");
                dv.paragraph(rows.where((row) => row.role === "editor").name.array().join(", "));
                dv.view("views/status-table.js", { title: "Statuses" });
                "#,
                Some("Dashboard.md"),
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
                dv.paragraph(dv.queryMarkdown('LIST FROM "Projects" SORT file.name ASC'));
                dv.current().status
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.outputs,
                vec![DataviewJsOutput::Paragraph {
                    text: "- [[Projects/Alpha]]\n- [[Projects/Beta]]".to_string(),
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
                vault_root.join(".vulcan/config.toml"),
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
                        "vault.search(query: string, opts?: { limit?: number }): SearchReport"
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
        fn dataviewjs_rejects_paths_outside_the_vault() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            std::fs::create_dir_all(vault_root.join(".vulcan"))
                .expect(".vulcan dir should be created");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let error = evaluate_dataview_js_query(
                &paths,
                r#"dv.io.load("../outside.md")"#,
                Some("Dashboard.md"),
            )
            .expect_err("DataviewJS should reject vault escapes");
            assert!(matches!(
                error,
                DataviewJsError::Message(message)
                    if message.contains("must stay inside the vault root")
            ));
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

            let result = evaluate_dataview_js_query(
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

            let result = evaluate_dataview_js_query(
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

            let result = evaluate_dataview_js_query(
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
                    vault.append("Scratch", "Follow-up", { heading: "## Log" });
                    vault.update("Scratch", "owner", "alice");
                    vault.unset("Scratch", "status");
                    created.name
                    "###,
                )
                .expect("fs sandbox mutations should succeed");
            assert_eq!(result.value, Some(Value::String("Scratch".to_string())));

            let scratch = fs::read_to_string(vault_root.join("Scratch.md"))
                .expect("scratch note should exist");
            assert!(scratch.contains("owner: alice"));
            assert!(!scratch.contains("status: draft"));
            assert!(scratch.contains("## Log"));
            assert!(scratch.contains("Follow-up"));

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
                vault_root.join(".vulcan/config.toml"),
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
        fn dataviewjs_reports_runtime_disabled_primitives() {
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
                    typeof eval === "undefined" ? "no-eval" : "eval",
                    typeof Function === "undefined" ? "no-function" : "function"
                  ].join(", ")
                )
                "#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.outputs,
                vec![DataviewJsOutput::Paragraph {
                    text: "no-fetch, no-eval, no-function".to_string(),
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
                    assert_eq!(rows[0][3], Value::String("People/Bob".to_string()));
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
                    timeout: Some(Duration::from_secs(1)),
                    sandbox: Some(JsRuntimeSandbox::Strict),
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
                    timeout: Some(Duration::from_secs(1)),
                    sandbox: Some(JsRuntimeSandbox::Strict),
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
                    timeout: Some(Duration::from_secs(1)),
                    sandbox: Some(JsRuntimeSandbox::Strict),
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
                    sandbox: Some(JsRuntimeSandbox::Strict),
                    permission_profile: Some("exec_only".to_string()),
                    ..DataviewJsEvalOptions::default()
                },
            )
            .expect("timed-out host.exec should still return a report");
            assert!(started.elapsed() < Duration::from_secs(1));
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
                    timeout: Some(Duration::from_secs(1)),
                    sandbox: Some(JsRuntimeSandbox::Strict),
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
