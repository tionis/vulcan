use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::dql::DqlQueryResult;
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

    use super::{evaluate_dataview_js_query, DataviewJsError};
    use crate::VaultPaths;

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

    use super::{DataviewJsError, DataviewJsOutput, DataviewJsResult};
    use crate::config::load_vault_config;
    use crate::dql::{evaluate_dql, DqlQueryResult};
    use crate::expression::eval::{compare_values, value_to_display};
    use crate::expression::functions::{
        format_date, format_duration, link_meta_value, parse_date_like_string,
        parse_duration_string,
    };
    use crate::file_metadata::FileMetadataResolver;
    use crate::graph::{
        query_graph_components, query_graph_dead_ends, query_graph_hubs, query_graph_path,
    };
    use crate::periodic::{
        list_daily_note_events, list_events_between, load_events_for_periodic_note,
        resolve_periodic_note, today_utc_string,
    };
    use crate::properties::{load_note_index, NoteRecord};
    use crate::resolve_note_reference;
    use crate::search::{search_vault, SearchQuery};
    use crate::VaultPaths;
    use serde_json::{Map, Value};
    use std::cmp::Ordering;

    #[derive(Debug)]
    struct JsEvalState {
        paths: VaultPaths,
        current_file: Option<String>,
        note_index: std::collections::HashMap<String, NoteRecord>,
        periodic_config: crate::PeriodicConfig,
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
      return __vulcanPrivateEval(source);
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
  func: null,
  luxon: {
    DateTime: VulcanDateTime,
    Duration: VulcanDuration,
  },
};

dv.func = buildDvFunctions(dv);
const vault = {
  note(path) {
    return dv.page(path);
  },
  notes(source) {
    return dv.pages(source);
  },
  query(dql, opts = {}) {
    return dv.tryQuery(dql, opts?.file ?? null, opts);
  },
  search(query, opts = {}) {
    return JSON.parse(__vulcan_search_json(String(query), opts?.limit ?? null));
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
      return JSON.parse(__vulcan_vault_daily_json(__vulcan_today()));
    },
    get(date) {
      return JSON.parse(__vulcan_vault_daily_json(String(date)));
    },
    range(from, to) {
      return new DataArray(JSON.parse(__vulcan_vault_daily_range_json(String(from), String(to))));
    },
  },
  events(options = {}) {
    return new DataArray(
      JSON.parse(__vulcan_vault_events_json(options?.from ?? null, options?.to ?? null))
    );
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

function help(obj) {
  if (obj === vault.note) {
    return "vault.note(path): note page object for one note.";
  }
  if (obj === vault.notes) {
    return "vault.notes(source?): DataArray of note page objects.";
  }
  if (obj === vault.query) {
    return "vault.query(dql, opts?): execute one DQL query.";
  }
  if (obj === vault.search) {
    return "vault.search(query, opts?): run one indexed search query.";
  }
  if (obj === vault.graph.shortestPath) {
    return "vault.graph.shortestPath(from, to): shortest resolved path between notes.";
  }
  if (obj === vault.daily.today) {
    return "vault.daily.today(): today\\'s daily note object with parsed events.";
  }
  if (obj === vault.events) {
    return "vault.events({ from, to }): DataArray of periodic events.";
  }
  return "No help available for this object.";
}

globalThis.dv = dv;
globalThis.vault = vault;
globalThis.console = console;
globalThis.help = help;
globalThis.this = dv.current();
globalThis.eval = undefined;
globalThis.Function = undefined;
"#;

    pub fn evaluate_dataview_js(
        paths: &VaultPaths,
        source: &str,
        current_file: Option<&str>,
    ) -> Result<DataviewJsResult, DataviewJsError> {
        let loaded_config = load_vault_config(paths).config;
        if !loaded_config.dataview.enable_dataview_js {
            return Err(DataviewJsError::Disabled);
        }

        let note_index =
            load_note_index(paths).map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let state = Arc::new(JsEvalState {
            paths: paths.clone(),
            current_file: current_file.map(ToOwned::to_owned),
            note_index,
            periodic_config: loaded_config.periodic.clone(),
        });
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let runtime =
            Runtime::new().map_err(|error| DataviewJsError::Message(error.to_string()))?;
        runtime.set_memory_limit(loaded_config.dataview.js_memory_limit_bytes);
        runtime.set_max_stack_size(loaded_config.dataview.js_max_stack_size_bytes);

        let deadline = Instant::now();
        let timeout_seconds = loaded_config.dataview.js_timeout_seconds;
        runtime.set_interrupt_handler(Some(Box::new(move || {
            deadline.elapsed().as_secs() >= u64::try_from(timeout_seconds).unwrap_or(u64::MAX)
        })));

        let context =
            Context::full(&runtime).map_err(|error| DataviewJsError::Message(error.to_string()))?;
        let eval_result = context.with(|ctx| -> Result<Option<Value>, DataviewJsError> {
            install_dataview_globals(ctx.clone(), Arc::clone(&state), Arc::clone(&outputs))?;
            ctx.eval::<(), _>(DATAVIEW_JS_PRELUDE)
                .catch(&ctx)
                .map_err(|error| map_caught_runtime_error(&error, timeout_seconds))?;
            let value: JsValue<'_> = ctx
                .eval(source)
                .catch(&ctx)
                .map_err(|error| map_caught_runtime_error(&error, timeout_seconds))?;
            if value.is_undefined() {
                Ok(None)
            } else {
                let serialize_fn: rquickjs::Function<'_> =
                    ctx.globals()
                        .get("__vulcanSerialize")
                        .map_err(|error| DataviewJsError::Message(error.to_string()))?;
                let serialized_json: String = serialize_fn
                    .call((value,))
                    .catch(&ctx)
                    .map_err(|error| map_caught_runtime_error(&error, timeout_seconds))?;
                serde_json::from_str(&serialized_json)
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

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
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

        let single_page_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_page_json",
                Func::from(move |ctx: Ctx<'_>, path: String| {
                    to_json_string(
                        &ctx,
                        page_object_by_reference(
                            &single_page_state.paths,
                            &single_page_state.note_index,
                            &path,
                        )
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

        globals
            .set("__vulcan_today", Func::from(today_utc_string))
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_daily_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_daily_json",
                Func::from(move |ctx: Ctx<'_>, date: String| {
                    to_json_string(
                        &ctx,
                        load_daily_page_object(
                            &vault_daily_state.paths,
                            &vault_daily_state.periodic_config,
                            &vault_daily_state.note_index,
                            &date,
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
                    )
                }),
            )
            .map_err(|error| DataviewJsError::Message(error.to_string()))?;

        let vault_daily_range_state = Arc::clone(&state);
        globals
            .set(
                "__vulcan_vault_daily_range_json",
                Func::from(move |ctx: Ctx<'_>, from: String, to: String| {
                    to_json_string(
                        &ctx,
                        load_daily_range_objects(
                            &vault_daily_range_state.paths,
                            &vault_daily_range_state.note_index,
                            &from,
                            &to,
                        )
                        .map_err(|error| Exception::throw_message(&ctx, &error.to_string()))?,
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
                        let (from, to) =
                            normalize_daily_event_range(from.as_deref(), to.as_deref()).map_err(
                                |error| Exception::throw_message(&ctx, &error.to_string()),
                            )?;
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
                    to_json_string(
                        &ctx,
                        search_vault(
                            &search_state.paths,
                            &SearchQuery {
                                text: query,
                                limit,
                                ..SearchQuery::default()
                            },
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
                    to_json_string(
                        &ctx,
                        query_graph_path(&graph_path_state.paths, &from, &to)
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
                    let mut report = query_graph_hubs(&graph_hubs_state.paths)
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
                    let mut report = query_graph_components(&graph_components_state.paths)
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
                    let mut report = query_graph_dead_ends(&graph_dead_ends_state.paths)
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
        Ok(note_by_path(note_index, &resolved.path).map_or(Value::Null, page_object))
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
    ) -> Result<Value, DataviewJsError> {
        let normalized_date = normalize_daily_event_date(Some(date))?;
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
    ) -> Result<Vec<Value>, DataviewJsError> {
        let (from, to) = normalize_daily_event_range(Some(from), Some(to))?;
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
    ) -> Result<(String, String), DataviewJsError> {
        let today = today_utc_string();
        let from_date = match from {
            Some(value) => normalize_daily_event_date(Some(value))?,
            None if to.is_some() => normalize_daily_event_date(to)?,
            None => today.clone(),
        };
        let to_date = match to {
            Some(value) => normalize_daily_event_date(Some(value))?,
            None if from.is_some() => from_date.clone(),
            None => today,
        };
        if from_date > to_date {
            return Err(DataviewJsError::Message(format!(
                "start date must be before or equal to end date: {from_date} > {to_date}"
            )));
        }
        Ok((from_date, to_date))
    }

    fn normalize_daily_event_date(raw: Option<&str>) -> Result<String, DataviewJsError> {
        let value = raw
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("today");
        if value.eq_ignore_ascii_case("today") {
            return Ok(today_utc_string());
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

    fn map_runtime_message(message: &str, timeout_seconds: usize) -> DataviewJsError {
        if message.to_ascii_lowercase().contains("interrupted") {
            DataviewJsError::Message(format!(
                "DataviewJS execution timed out after {timeout_seconds} second(s)"
            ))
        } else {
            DataviewJsError::Message(message.trim().to_string())
        }
    }

    fn map_caught_runtime_error(
        error: &CaughtError<'_>,
        timeout_seconds: usize,
    ) -> DataviewJsError {
        map_runtime_message(&error.to_string(), timeout_seconds)
    }

    fn drain_pending_jobs(
        runtime: &Runtime,
        timeout_seconds: usize,
    ) -> Result<(), DataviewJsError> {
        while runtime.is_job_pending() {
            runtime.execute_pending_job().map_err(|error| {
                error.0.with(|ctx| {
                    map_caught_runtime_error(
                        &CaughtError::from_error(&ctx, rquickjs::Error::Exception),
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
        fn dataviewjs_exposes_vault_daily_namespace_and_events() {
            let temp_dir = tempdir().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
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

            assert_eq!(
                result.outputs[0],
                DataviewJsOutput::Paragraph {
                    text: "vault.search(query, opts?): run one indexed search query.".to_string(),
                }
            );
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
