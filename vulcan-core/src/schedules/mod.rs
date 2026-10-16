//! Vault-owned schedules (Roadmap 10.8, `docs/specs/scheduled-execution.md`,
//! schema v1): job definitions read from the shared `.vulcan/config.toml`.
//!
//! Jobs are canonical vault configuration, so they travel with the vault. A
//! device-local `config.local.toml` cannot create or enable one; its
//! `[schedules]` table is reported and ignored. A malformed job blocks only
//! itself; a malformed `[schedules]` table, an unsupported version, or a
//! conflicting duplicate ID blocks every job it could affect.

pub mod time;

use crate::paths::VaultPaths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use time::{parse_interval, parse_timezone, CronExpression, Trigger};

pub const SCHEDULE_SCHEMA_VERSION: u32 = 1;
const MAX_JOBS: usize = 256;
const MAX_NODES: usize = 32;
const MAX_JOB_ID_BYTES: usize = 64;
pub const DEFAULT_MAX_RUNTIME_SECONDS: u64 = 3_600;
const MAX_RUNTIME_SECONDS: u64 = 86_400;
const MAX_RETRIES: u8 = 5;

/// The typed operation a job runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "builtin", rename_all = "kebab-case")]
pub enum ScheduleTarget {
    /// An incremental scan, or a full one.
    Scan { full: bool },
    /// The vault's configured batched commit (`vulcan git batch-commit`).
    BatchCommit,
}

impl ScheduleTarget {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Scan { .. } => "scan",
            Self::BatchCommit => "batch-commit",
        }
    }

    /// Whether running the target twice for one occurrence is harmless, so
    /// an interrupted run may be retried.
    #[must_use]
    pub fn idempotent(&self) -> bool {
        match self {
            Self::Scan { .. } | Self::BatchCommit => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleLimits {
    pub max_runtime_seconds: u64,
    pub retries: u8,
}

/// One validated job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleJob {
    pub id: String,
    pub enabled: bool,
    pub target: ScheduleTarget,
    /// Exact device IDs that run this job.
    pub nodes: Vec<String>,
    #[serde(serialize_with = "serialize_trigger")]
    pub trigger: Trigger,
    /// The permission profile the job runs under, capped by the device.
    pub authority: String,
    pub limits: ScheduleLimits,
    /// BLAKE3 of the job's canonical definition: a changed definition is a
    /// new revision whose occurrences never collide with the old one's.
    pub revision: String,
}

impl ScheduleJob {
    #[must_use]
    pub fn runs_on(&self, device_id: &str) -> bool {
        self.nodes.iter().any(|node| node == device_id)
    }

    /// The occurrence to dispatch at `now`, given `watermark`: the latest
    /// occurrence already dispatched, or when this revision was first seen.
    /// It is the latest occurrence in `(watermark, now]`, so occurrences
    /// missed during downtime, suspend, or a forward clock jump coalesce into
    /// one run, and a backward jump repeats nothing.
    #[must_use]
    pub fn due(&self, watermark: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.trigger
            .latest_at_or_before(now)
            .filter(|occurrence| *occurrence > watermark)
    }

    /// The next `count` occurrences after `after`.
    #[must_use]
    pub fn next_occurrences(&self, after: DateTime<Utc>, count: usize) -> Vec<DateTime<Utc>> {
        let mut occurrences = Vec::with_capacity(count);
        let mut cursor = after;
        while occurrences.len() < count {
            let Some(next) = self.trigger.next_after(cursor) else {
                break;
            };
            occurrences.push(next);
            cursor = next;
        }
        occurrences
    }
}

fn serialize_trigger<S: serde::Serializer>(
    trigger: &Trigger,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    trigger_json(trigger).serialize(serializer)
}

fn trigger_json(trigger: &Trigger) -> serde_json::Value {
    match trigger {
        Trigger::Every {
            every_seconds,
            anchor,
        } => serde_json::json!({ "every_seconds": every_seconds, "anchor": anchor.to_rfc3339() }),
        Trigger::Cron {
            expression,
            timezone,
        } => serde_json::json!({ "cron": expression.source(), "timezone": timezone.name() }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduleDiagnostic {
    /// The job the problem blocks, or `None` when it blocks the section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    pub message: String,
}

/// The vault's schedules as this device reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScheduleSet {
    pub jobs: Vec<ScheduleJob>,
    /// Jobs and sections that cannot run, and why.
    pub diagnostics: Vec<ScheduleDiagnostic>,
    /// Set when no job may run: the section itself is unusable.
    pub blocked: bool,
}

impl ScheduleSet {
    #[must_use]
    pub fn job(&self, id: &str) -> Option<&ScheduleJob> {
        self.jobs.iter().find(|job| job.id == id)
    }

    /// Jobs blocked by a diagnostic, by ID.
    #[must_use]
    pub fn blocked_job_ids(&self) -> BTreeSet<String> {
        self.diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.job.clone())
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedules {
    version: Option<u32>,
    #[serde(default)]
    jobs: Vec<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJob {
    id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    target: RawTarget,
    #[serde(default)]
    input: toml::Table,
    nodes: Vec<String>,
    trigger: RawTrigger,
    authority: String,
    #[serde(default)]
    limits: RawLimits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTarget {
    builtin: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrigger {
    every: Option<String>,
    anchor: Option<toml::Value>,
    cron: Option<String>,
    timezone: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    max_runtime_seconds: Option<u64>,
    retries: Option<u8>,
}

const fn default_true() -> bool {
    true
}

/// Reads the vault's schedules from `.vulcan/config.toml`. A missing file
/// or a file without `[schedules]` has no jobs.
#[must_use]
pub fn load_schedules(paths: &VaultPaths) -> ScheduleSet {
    let mut set = ScheduleSet::default();
    if let Ok(local) = std::fs::read_to_string(paths.local_config_file()) {
        if toml::from_str::<toml::Table>(&local).is_ok_and(|table| table.contains_key("schedules"))
        {
            set.diagnostics.push(ScheduleDiagnostic {
                job: None,
                message: "`[schedules]` in config.local.toml is ignored: jobs are vault configuration and belong in config.toml".to_string(),
            });
        }
    }
    let source = match std::fs::read_to_string(paths.config_file()) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return set,
        Err(error) => return block(set, format!("cannot read config.toml: {error}")),
    };
    let table = match toml::from_str::<toml::Table>(&source) {
        Ok(table) => table,
        Err(error) => return block(set, format!("config.toml does not parse: {error}")),
    };
    let Some(section) = table.get("schedules") else {
        return set;
    };
    let raw = match section.clone().try_into::<RawSchedules>() {
        Ok(raw) => raw,
        Err(error) => return block(set, format!("`[schedules]` is invalid: {error}")),
    };
    match raw.version {
        Some(SCHEDULE_SCHEMA_VERSION) => {}
        Some(other) => {
            return block(
                set,
                format!(
                    "`[schedules] version = {other}` is not supported; this Vulcan reads version {SCHEDULE_SCHEMA_VERSION}"
                ),
            )
        }
        None => {
            return block(
                set,
                format!("`[schedules]` needs `version = {SCHEDULE_SCHEMA_VERSION}`"),
            )
        }
    }
    if raw.jobs.len() > MAX_JOBS {
        return block(set, format!("at most {MAX_JOBS} jobs may be scheduled"));
    }
    let mut by_id = BTreeMap::<String, usize>::new();
    for value in raw.jobs {
        let id_hint = value
            .get("id")
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        match parse_job(value) {
            Ok(job) => {
                *by_id.entry(job.id.clone()).or_default() += 1;
                set.jobs.push(job);
            }
            Err(message) => {
                if let Some(id) = &id_hint {
                    *by_id.entry(id.clone()).or_default() += 1;
                }
                set.diagnostics.push(ScheduleDiagnostic {
                    job: id_hint.clone(),
                    message,
                });
                if id_hint.is_none() {
                    // A job without a usable ID cannot be told apart from the
                    // others, so none may run.
                    set.blocked = true;
                }
            }
        }
    }
    let duplicates = by_id
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id)
        .collect::<BTreeSet<_>>();
    for id in &duplicates {
        set.diagnostics.push(ScheduleDiagnostic {
            job: Some(id.clone()),
            message: format!(
                "job ID `{id}` is defined more than once; none of its definitions run"
            ),
        });
    }
    let blocked = set.blocked_job_ids();
    set.jobs.retain(|job| !blocked.contains(&job.id));
    if set.blocked {
        set.jobs.clear();
    }
    set
}

fn block(mut set: ScheduleSet, message: String) -> ScheduleSet {
    set.diagnostics
        .push(ScheduleDiagnostic { job: None, message });
    set.blocked = true;
    set.jobs.clear();
    set
}

fn parse_job(value: toml::Value) -> Result<ScheduleJob, String> {
    let raw: RawJob = value
        .try_into()
        .map_err(|error| format!("invalid job: {error}"))?;
    validate_job_id(&raw.id)?;
    let target = parse_target(&raw.target.builtin, raw.input)?;
    if raw.nodes.is_empty() {
        return Err(
            "`nodes` must name at least one device ID (see `vulcan device show`)".to_string(),
        );
    }
    if raw.nodes.len() > MAX_NODES {
        return Err(format!("at most {MAX_NODES} nodes may run one job"));
    }
    let mut nodes = Vec::with_capacity(raw.nodes.len());
    for node in raw.nodes {
        if node.is_empty()
            || node.len() > 128
            || !node
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(format!("`{node}` is not a device ID"));
        }
        if nodes.contains(&node) {
            return Err(format!("node `{node}` is listed twice"));
        }
        nodes.push(node);
    }
    let trigger = parse_trigger(raw.trigger)?;
    if raw.authority.trim().is_empty() {
        return Err("`authority` must name a permission profile".to_string());
    }
    let max_runtime_seconds = raw
        .limits
        .max_runtime_seconds
        .unwrap_or(DEFAULT_MAX_RUNTIME_SECONDS);
    if !(1..=MAX_RUNTIME_SECONDS).contains(&max_runtime_seconds) {
        return Err(format!(
            "`limits.max_runtime_seconds` must be 1 to {MAX_RUNTIME_SECONDS}"
        ));
    }
    let retries = raw.limits.retries.unwrap_or(0);
    if retries > MAX_RETRIES {
        return Err(format!("`limits.retries` must be 0 to {MAX_RETRIES}"));
    }
    if retries > 0 && !target.idempotent() {
        return Err(format!(
            "`{}` cannot be retried: it is not idempotent",
            target.name()
        ));
    }
    let mut job = ScheduleJob {
        id: raw.id,
        enabled: raw.enabled,
        target,
        nodes,
        trigger,
        authority: raw.authority,
        limits: ScheduleLimits {
            max_runtime_seconds,
            retries,
        },
        revision: String::new(),
    };
    job.revision = revision(&job);
    Ok(job)
}

fn validate_job_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > MAX_JOB_ID_BYTES
        || !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        return Err(format!(
            "job ID `{id}` must be 1 to {MAX_JOB_ID_BYTES} of a-z, 0-9, `_`, and `-`"
        ));
    }
    Ok(())
}

fn parse_target(builtin: &str, mut input: toml::Table) -> Result<ScheduleTarget, String> {
    let target = match builtin {
        "scan" => {
            let full = match input.remove("full") {
                None => false,
                Some(toml::Value::Boolean(full)) => full,
                Some(_) => return Err("`input.full` must be true or false".to_string()),
            };
            ScheduleTarget::Scan { full }
        }
        "batch-commit" => ScheduleTarget::BatchCommit,
        other => {
            return Err(format!(
                "unknown target `{other}`; v1 runs `scan` and `batch-commit`"
            ))
        }
    };
    if let Some(key) = input.keys().next() {
        return Err(format!("`{builtin}` takes no input `{key}`"));
    }
    Ok(target)
}

fn parse_trigger(raw: RawTrigger) -> Result<Trigger, String> {
    match (raw.every, raw.cron) {
        (Some(every), None) => {
            if raw.timezone.is_some() {
                return Err("`timezone` applies to `cron` triggers only".to_string());
            }
            let every_seconds = parse_interval(&every).map_err(|error| error.to_string())?;
            let anchor = match raw.anchor {
                None => DateTime::<Utc>::UNIX_EPOCH,
                Some(anchor) => {
                    let text = match anchor {
                        toml::Value::Datetime(datetime) => datetime.to_string(),
                        toml::Value::String(text) => text,
                        other => other.to_string(),
                    };
                    DateTime::parse_from_rfc3339(&text)
                        .map_err(|_| {
                            format!("`anchor` must be a date-time with an offset, such as 2026-01-01T00:00:00Z, not `{text}`")
                        })?
                        .with_timezone(&Utc)
                }
            };
            Ok(Trigger::Every {
                every_seconds,
                anchor,
            })
        }
        (None, Some(cron)) => {
            if raw.anchor.is_some() {
                return Err("`anchor` applies to `every` triggers only".to_string());
            }
            let timezone = raw.timezone.ok_or_else(|| {
                "a `cron` trigger needs a `timezone`, such as \"Europe/Berlin\" or \"UTC\""
                    .to_string()
            })?;
            Ok(Trigger::Cron {
                expression: CronExpression::parse(&cron).map_err(|error| error.to_string())?,
                timezone: parse_timezone(&timezone).map_err(|error| error.to_string())?,
            })
        }
        (Some(_), Some(_)) => Err("a trigger has `every` or `cron`, not both".to_string()),
        (None, None) => Err("a trigger needs `every` or `cron`".to_string()),
    }
}

fn revision(job: &ScheduleJob) -> String {
    // Canonical: fixed field order and every field that changes behavior.
    let canonical = serde_json::json!({
        "id": job.id,
        "enabled": job.enabled,
        "target": job.target,
        "nodes": job.nodes,
        "trigger": trigger_json(&job.trigger),
        "authority": job.authority,
        "limits": job.limits,
    });
    blake3::hash(canonical.to_string().as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(config: &str) -> (tempfile::TempDir, VaultPaths) {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        std::fs::write(paths.config_file(), config).unwrap();
        (temporary, paths)
    }

    const JOB: &str = r#"
[schedules]
version = 1

[[schedules.jobs]]
id = "nightly-commit"
target = { builtin = "batch-commit" }
nodes = ["device-a"]
trigger = { cron = "30 2 * * *", timezone = "Europe/Berlin" }
authority = "unrestricted"

[[schedules.jobs]]
id = "rescan"
enabled = false
target = { builtin = "scan" }
input = { full = true }
nodes = ["device-a", "vdev1_ijcdxdmwcil6kw3i7zrksqg4kiejiqfyjhjg6bt4pwlqcqxvxbwq"]
trigger = { every = "15m", anchor = 2026-01-01T00:00:00Z }
authority = "readonly"
limits = { max_runtime_seconds = 120, retries = 2 }
"#;

    #[test]
    fn valid_jobs_load_with_their_targets_triggers_and_revisions() {
        let (_temporary, paths) = vault(JOB);
        let set = load_schedules(&paths);
        assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
        assert_eq!(set.jobs.len(), 2);
        let commit = set.job("nightly-commit").unwrap();
        assert!(commit.enabled);
        assert_eq!(commit.target, ScheduleTarget::BatchCommit);
        assert_eq!(
            commit.limits.max_runtime_seconds,
            DEFAULT_MAX_RUNTIME_SECONDS
        );
        assert!(commit.runs_on("device-a") && !commit.runs_on("device-b"));
        let rescan = set.job("rescan").unwrap();
        assert!(!rescan.enabled);
        assert_eq!(rescan.target, ScheduleTarget::Scan { full: true });
        assert_eq!(rescan.limits.retries, 2);
        assert!(rescan.runs_on("vdev1_ijcdxdmwcil6kw3i7zrksqg4kiejiqfyjhjg6bt4pwlqcqxvxbwq"));
        assert!(matches!(
            rescan.trigger,
            Trigger::Every {
                every_seconds: 900,
                ..
            }
        ));
        assert_eq!(commit.revision.len(), 64);
        assert_ne!(commit.revision, rescan.revision);
        // The revision follows the definition, not the file's formatting.
        let (_other, reformatted) = vault(&JOB.replace("  ", " ").replace("\n\n", "\n"));
        assert_eq!(
            load_schedules(&reformatted)
                .job("nightly-commit")
                .unwrap()
                .revision,
            commit.revision
        );
        let (_changed, changed) = vault(&JOB.replace("30 2 * * *", "45 2 * * *"));
        assert_ne!(
            load_schedules(&changed)
                .job("nightly-commit")
                .unwrap()
                .revision,
            commit.revision
        );
    }

    #[test]
    fn an_invalid_job_blocks_only_itself() {
        let (_temporary, paths) = vault(&format!(
            "{JOB}\n[[schedules.jobs]]\nid = \"broken\"\ntarget = {{ builtin = \"rss\" }}\nnodes = [\"device-a\"]\ntrigger = {{ every = \"15m\" }}\nauthority = \"unrestricted\"\n"
        ));
        let set = load_schedules(&paths);
        assert_eq!(set.jobs.len(), 2);
        assert!(!set.blocked);
        assert_eq!(set.diagnostics[0].job.as_deref(), Some("broken"));
        assert!(set.diagnostics[0].message.contains("unknown target"));
    }

    #[test]
    fn invalid_definitions_are_explained() {
        for (job, expected) in [
            ("id = \"Bad ID\"", "job ID"),
            ("nodes = []", "at least one device ID"),
            ("trigger = { every = \"10s\" }", "outside 1 minute"),
            ("trigger = { cron = \"30 2 * * *\" }", "needs a `timezone`"),
            (
                "trigger = { every = \"15m\", cron = \"* * * * *\" }",
                "not both",
            ),
            (
                "trigger = { cron = \"* * * *\", timezone = \"UTC\" }",
                "five fields",
            ),
            (
                "trigger = { cron = \"* * * * *\", timezone = \"Mars/Base\" }",
                "IANA",
            ),
            ("input = { depth = 3 }", "takes no input"),
            ("limits = { retries = 9 }", "0 to 5"),
            (
                "limits = { max_runtime_seconds = 0 }",
                "max_runtime_seconds",
            ),
            ("authority = \" \"", "permission profile"),
            ("surprise = true", "unknown field"),
        ] {
            let mut fields = BTreeMap::from([
                ("id", "id = \"job\"".to_string()),
                (
                    "target",
                    "target = { builtin = \"batch-commit\" }".to_string(),
                ),
                ("nodes", "nodes = [\"device-a\"]".to_string()),
                ("trigger", "trigger = { every = \"15m\" }".to_string()),
                ("authority", "authority = \"unrestricted\"".to_string()),
            ]);
            let key = job.split(' ').next().unwrap();
            fields.insert(
                match key {
                    "id" | "nodes" | "trigger" | "authority" => key,
                    other => other,
                },
                job.to_string(),
            );
            let body = fields.values().cloned().collect::<Vec<_>>().join("\n");
            let (_temporary, paths) = vault(&format!(
                "[schedules]\nversion = 1\n[[schedules.jobs]]\n{body}\n"
            ));
            let set = load_schedules(&paths);
            assert!(set.jobs.is_empty(), "{job}");
            let message = &set.diagnostics.first().expect(job).message;
            assert!(message.contains(expected), "{job}: {message}");
        }
    }

    #[test]
    fn duplicate_ids_versions_and_local_tables_block_what_they_affect() {
        let duplicate = JOB.replace("id = \"rescan\"", "id = \"nightly-commit\"");
        let (_temporary, paths) = vault(&duplicate);
        let set = load_schedules(&paths);
        assert!(set.jobs.is_empty());
        assert!(set.diagnostics[0].message.contains("more than once"));

        for (config, expected) in [
            (JOB.replace("version = 1", "version = 2"), "not supported"),
            (JOB.replace("version = 1\n", ""), "needs `version = 1`"),
            (
                "[schedules]\nversion = 1\njobs = 3\n".to_string(),
                "invalid",
            ),
        ] {
            let (_temporary, paths) = vault(&config);
            let set = load_schedules(&paths);
            assert!(set.blocked && set.jobs.is_empty(), "{config}");
            assert!(
                set.diagnostics[0].message.contains(expected),
                "{:?}",
                set.diagnostics
            );
        }

        let (_temporary, paths) = vault(JOB);
        std::fs::write(
            paths.local_config_file(),
            "[schedules]\nversion = 1\n[[schedules.jobs]]\nid = \"local\"\n",
        )
        .unwrap();
        let set = load_schedules(&paths);
        assert_eq!(set.jobs.len(), 2, "shared jobs still load");
        assert!(set.job("local").is_none());
        assert!(set.diagnostics[0]
            .message
            .contains("config.local.toml is ignored"));
    }

    #[test]
    fn missed_occurrences_coalesce_and_clock_jumps_repeat_nothing() {
        let (_temporary, paths) = vault(JOB);
        let set = load_schedules(&paths);
        let rescan = set.job("rescan").unwrap();
        let at = |text: &str| {
            DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&Utc)
        };
        let activated = at("2026-03-04T10:07:00Z");
        assert_eq!(
            rescan.due(activated, activated),
            None,
            "nothing before activation"
        );
        assert_eq!(
            rescan.due(activated, at("2026-03-04T10:15:00Z")),
            Some(at("2026-03-04T10:15:00Z"))
        );
        // Three hours down: one run for the latest missed occurrence.
        let dispatched = at("2026-03-04T10:15:00Z");
        assert_eq!(
            rescan.due(dispatched, at("2026-03-04T13:20:00Z")),
            Some(at("2026-03-04T13:15:00Z"))
        );
        // The clock jumps back an hour: nothing repeats.
        assert_eq!(
            rescan.due(at("2026-03-04T13:15:00Z"), at("2026-03-04T12:20:00Z")),
            None
        );
        assert_eq!(
            rescan.next_occurrences(dispatched, 3),
            [
                at("2026-03-04T10:30:00Z"),
                at("2026-03-04T10:45:00Z"),
                at("2026-03-04T11:00:00Z")
            ]
        );
    }

    #[test]
    fn a_vault_without_schedules_has_no_jobs() {
        let (_temporary, paths) = vault("[git]\nauto_commit = true\n");
        assert_eq!(load_schedules(&paths), ScheduleSet::default());
        let temporary = tempfile::tempdir().unwrap();
        assert_eq!(
            load_schedules(&VaultPaths::new(temporary.path())),
            ScheduleSet::default()
        );
    }
}
