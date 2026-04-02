use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(feature = "js_runtime")]
use std::cmp::Ordering;

#[cfg(feature = "js_runtime")]
use crate::config::load_vault_config;
#[cfg(feature = "js_runtime")]
use crate::dql::evaluate_dql;
use crate::dql::DqlQueryResult;
#[cfg(feature = "js_runtime")]
use crate::expression::eval::{compare_values, value_to_display};
#[cfg(feature = "js_runtime")]
use crate::expression::functions::{parse_date_like_string, parse_duration_string};
#[cfg(feature = "js_runtime")]
use crate::file_metadata::FileMetadataResolver;
#[cfg(feature = "js_runtime")]
use crate::properties::{load_note_index, NoteRecord};
#[cfg(feature = "js_runtime")]
use crate::resolve_note_reference;
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
    _paths: &VaultPaths,
    _source: &str,
    _current_file: Option<&str>,
) -> Result<DataviewJsResult, DataviewJsError> {
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

#[cfg(all(test, not(feature = "js_runtime")))]
mod disabled_tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use crate::{scan_vault, ScanMode};

    use super::*;

    #[test]
    fn dataviewjs_requires_runtime_feature() {
        let temp_dir = tempdir().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
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
    use std::fs;
    use std::path::{Component, Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use csv::ReaderBuilder;
    use rquickjs::function::Func;
    use rquickjs::{
        CatchResultExt, CaughtError, Context, Ctx, Exception, Runtime, Value as JsValue,
    };
    use serde::de::DeserializeOwned;
    use serde::Serialize;

    use super::*;

    #[derive(Debug)]
    struct JsEvalState {
        paths: VaultPaths,
        current_file: Option<String>,
        note_index: std::collections::HashMap<String, NoteRecord>,
    }

    #[derive(Debug, Clone, Serialize)]
    struct QueryResponse {
        successful: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<DqlQueryResult>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    const DATAVIEW_JS_PRELUDE: &str = r#"
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
  if (value instanceof DataArray) {
    return value.array().map(__vulcanPlain);
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

const dv = {
  pages(source) {
    return new DataArray(JSON.parse(__vulcan_pages_json(source)));
  },
  page(path) {
    return JSON.parse(__vulcan_page_json(path));
  },
  current() {
    return JSON.parse(__vulcan_current_json());
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
      return eval(source);
    } finally {
      globalThis.input = previousInput;
    }
  },
  date(input) {
    const millis = __vulcan_date_millis(
      input instanceof Date ? input.toISOString() : String(input)
    );
    return millis == null ? null : new Date(millis);
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
  func: {},
  luxon: {},
};

globalThis.dv = dv;
globalThis.this = dv.current();
"#;

    pub fn evaluate_dataview_js(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        let config = load_vault_config(paths).config.dataview;
        if !config.enable_dataview_js {
            return Err(DataviewJsError::Disabled);
        }

        let note_index =
            load_note_index(paths).map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let state = Arc::new(JsEvalState {
            paths: paths.clone(),
            current_file: current_file.map(ToOwned::to_owned),
            note_index,
        });
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let runtime =
            Runtime::new().map_err(|error| DataviewJsError::Message(error.to_string()))?;
        runtime.set_memory_limit(config.js_memory_limit_bytes);
        runtime.set_max_stack_size(config.js_max_stack_size_bytes);

        let deadline = Instant::now();
        let timeout_seconds = config.js_timeout_seconds;
        runtime.set_interrupt_handler(Some(Box::new(move || {
            deadline.elapsed().as_secs() >= u64::try_from(timeout_seconds).unwrap_or(u64::MAX)
        })));

        let context =
            Context::full(&runtime).map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let eval_result = context.with(|ctx| -> Result<Option<Value>, DataviewJsError> {
            install_dataview_globals(ctx.clone(), Arc::clone(&state), Arc::clone(&outputs))?;
            ctx.eval::<(), _>(DATAVIEW_JS_PRELUDE)
                .catch(&ctx)
                .map_err(|error| map_caught_runtime_error(error, timeout_seconds))?;
            let value: JsValue<'_> = ctx
                .eval(source)
                .catch(&ctx)
                .map_err(|error| map_caught_runtime_error(error, timeout_seconds))?;
            if value.is_undefined() {
                Ok(None)
            } else {
                let serializer: rquickjs::Function<'_> = ctx
                    .globals()
                    .get("__vulcanSerialize")
                    .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                let serialized: String = serializer
                    .call((value,))
                    .catch(&ctx)
                    .map_err(|error| map_caught_runtime_error(error, timeout_seconds))?;
                serde_json::from_str(&serialized)
                    .map(Some)
                    .map_err(|error| DataviewJsError::Message(error.to_string()))
            }
        });

        let drain_result = drain_pending_jobs(&runtime, timeout_seconds);
        runtime.set_interrupt_handler(None);
        drain_result?;
        let value = eval_result?;

        Ok(DataviewJsResult {
            outputs: outputs
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default(),
            value,
        })
    }

    pub fn evaluate_dataview_js_query(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        evaluate_dataview_js(paths, source, current_file)
    }

    fn install_dataview_globals(
        ctx: Ctx<'_>,
        state: Arc<JsEvalState>,
        outputs: Arc<Mutex<Vec<DataviewJsOutput>>>,
    ) -> Result<(), DataviewJsError> {
        let globals = ctx.globals();

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
                    to_json_string(
                        &ctx,
                        load_pages_from_source(
                            &pages_state.paths,
                            &pages_state.note_index,
                            pages_state.current_file.as_deref(),
                            source.as_deref(),
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let page_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_page_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    to_json_string(
                        &ctx,
                        page_object_by_reference(&page_state.paths, &page_state.note_index, &path)
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let current_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_current_json",
                Func::from(move |ctx: Ctx<'_>| {
                    to_json_string(
                        &ctx,
                        current_state
                            .current_file
                            .as_deref()
                            .map(|path| {
                                page_object_by_reference(
                                    &current_state.paths,
                                    &current_state.note_index,
                                    path,
                                )
                            })
                            .transpose()
                            .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?
                            .unwrap_or(Value::Null),
                    )
                }),
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

        Ok(())
    }

    fn load_pages_from_source(
        paths: &VaultPaths,
        note_index: &std::collections::HashMap<String, NoteRecord>,
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
        let resolved = resolve_note_reference(paths, file)
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;
        Ok(note_by_path(note_index, &resolved.path)
            .map(page_object)
            .unwrap_or(Value::Null))
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

    fn map_runtime_message(message: String, timeout_seconds: usize) -> DataviewJsError {
        if message.to_ascii_lowercase().contains("interrupted") {
            DataviewJsError::Message(format!(
                "DataviewJS execution timed out after {timeout_seconds} second(s)"
            ))
        } else {
            DataviewJsError::Message(message.trim().to_string())
        }
    }

    fn map_caught_runtime_error(error: CaughtError<'_>, timeout_seconds: usize) -> DataviewJsError {
        map_runtime_message(error.to_string(), timeout_seconds)
    }

    fn drain_pending_jobs(
        runtime: &Runtime,
        timeout_seconds: usize,
    ) -> Result<(), DataviewJsError> {
        while runtime.is_job_pending() {
            runtime.execute_pending_job().map_err(|error| {
                error.0.with(|ctx| {
                    map_caught_runtime_error(
                        CaughtError::from_error(&ctx, rquickjs::Error::Exception),
                        timeout_seconds,
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
        use std::path::Path;
        use std::time::Duration;

        use tempfile::tempdir;

        use crate::{scan_vault, ScanMode};

        use super::*;

        #[test]
        fn dataviewjs_exposes_current_page_lookup_and_pages_helpers() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
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
        fn dataviewjs_rejects_paths_outside_the_vault() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
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
        fn dataviewjs_reports_runtime_disabled_primitives() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault("dataview", &vault_root);
            let paths = VaultPaths::new(&vault_root);
            scan_vault(&paths, ScanMode::Full).expect("vault should scan");

            let result = evaluate_dataview_js_query(
                &paths,
                r#"dv.paragraph(typeof fetch === "undefined" ? "no-fetch" : "fetch")"#,
                Some("Dashboard.md"),
            )
            .expect("DataviewJS should evaluate");

            assert_eq!(
                result.outputs,
                vec![DataviewJsOutput::Paragraph {
                    text: "no-fetch".to_string(),
                }]
            );
        }

        fn copy_fixture_vault(name: &str, destination: &Path) {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/vaults")
                .join(name);

            copy_dir_recursive(&source, destination);
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
}

#[cfg(feature = "js_runtime")]
pub use runtime::{evaluate_dataview_js, evaluate_dataview_js_query};
