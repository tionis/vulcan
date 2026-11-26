//! `vulcan metabind press`: runs a Meta Bind button's actions in order.
//! Text actions edit the in-memory notes; JavaScript actions (`js`,
//! `inlineJS`) run in a trusted vault with `metabind.enable_js`, after the
//! edits before them are written, and may write through the vault API.
//! Actions that need Obsidian's interface are reported, not run.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;
use vulcan_core::metabind::buttons::{
    find_button, insert_at_line, line_number, regexp_replace, replace_block, replace_lines,
    ButtonBlock, ButtonSelector, PressableButton,
};
use vulcan_core::metabind::{frontmatter_json, read_path, resolve_target_note, BindTarget};
use vulcan_core::{DataviewJsEvalOptions, DataviewJsSession, VaultConfig, VaultPaths};

use super::{write_value, MetaBindChange, MetaBindFileWrite, Scope};
use crate::AppError;

const PLAIN_PATH: vulcan_core::paths::RelativePathOptions =
    vulcan_core::paths::RelativePathOptions {
        expected_extension: None,
        append_extension_if_missing: false,
    };

/// Writes a pressed button's edits as they become due: before a
/// JavaScript action and at the end.
pub trait MetaBindWriter {
    fn write(&mut self, write: &MetaBindFileWrite) -> Result<(), AppError>;
}

/// Options for [`press_metabind_button`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MetaBindPressOptions<'a> {
    /// Plan only: nothing is written and JavaScript actions run read-only.
    pub dry_run: bool,
    pub permission_profile: Option<&'a str>,
}

/// What pressing a button did.
#[derive(Debug, Clone, Serialize)]
pub struct MetaBindPressReport {
    pub path: String,
    pub label: String,
    pub dry_run: bool,
    pub actions: Vec<MetaBindActionReport>,
    pub changes: Vec<MetaBindChange>,
    /// Notes written (or, for a dry run, that would be), in order.
    pub written: Vec<String>,
    /// Of those, the notes created.
    pub created: Vec<String>,
    /// The writes themselves, for editors that apply them.
    #[serde(skip)]
    pub writes: Vec<MetaBindFileWrite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetaBindActionReport {
    #[serde(rename = "type")]
    pub action_type: String,
    /// `false` for an action that runs only in Obsidian.
    pub ran: bool,
    pub detail: String,
}

struct Press<'a> {
    paths: &'a VaultPaths,
    config: &'a VaultConfig,
    scope: Scope,
    path: &'a str,
    /// The button's block as declared, to find it again after edits.
    block_text: Option<String>,
    button: PressableButton,
    options: MetaBindPressOptions<'a>,
    sources: BTreeMap<String, (String, String)>,
    created: BTreeSet<String>,
    report: MetaBindPressReport,
    writes: Vec<MetaBindFileWrite>,
}

/// Presses the button `selector` names in `source`, the note at `path`.
/// Edits reach `writer` before each JavaScript action and at the end;
/// with `dry_run` they are only reported.
pub fn press_metabind_button(
    paths: &VaultPaths,
    config: &VaultConfig,
    path: &str,
    source: &str,
    selector: &ButtonSelector,
    options: MetaBindPressOptions<'_>,
    writer: &mut dyn MetaBindWriter,
) -> Result<MetaBindPressReport, AppError> {
    if !config.metabind.enabled {
        return Err(AppError::operation(
            "Meta Bind support is off (`metabind.enabled`)",
        ));
    }
    let scope = Scope::load(paths, path, options.permission_profile)?;
    let parsed = vulcan_core::parse_document(source, config);
    let button =
        find_button(&parsed, source, &config.metabind, selector).map_err(AppError::operation)?;
    let actions = button.actions().map_err(AppError::operation)?;
    let block_text = button
        .block
        .as_ref()
        .and_then(|block| source.get(block.byte_range.clone()))
        .map(ToOwned::to_owned);
    let mut sources = BTreeMap::new();
    sources.insert(path.to_string(), (source.to_string(), source.to_string()));
    let mut press = Press {
        paths,
        config,
        scope,
        path,
        block_text,
        report: MetaBindPressReport {
            path: path.to_string(),
            label: button.label().to_string(),
            dry_run: options.dry_run,
            actions: Vec::new(),
            changes: Vec::new(),
            written: Vec::new(),
            created: Vec::new(),
            writes: Vec::new(),
        },
        button,
        options,
        sources,
        created: BTreeSet::new(),
        writes: Vec::new(),
    };
    for (index, action) in actions.iter().enumerate() {
        let action_type = action
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let outcome = press.run(&action_type, action, writer).map_err(|error| {
            AppError::operation(format!(
                "action {} (`{action_type}`): {}",
                index + 1,
                error.message()
            ))
        })?;
        press.report.actions.push(MetaBindActionReport {
            action_type,
            ran: outcome.0,
            detail: outcome.1,
        });
    }
    press.flush(writer)?;
    let mut report = press.report;
    for write in &press.writes {
        if write.created {
            report.created.push(write.path.clone());
        }
        if !report.written.contains(&write.path) {
            report.written.push(write.path.clone());
        }
    }
    report.writes = press.writes;
    Ok(report)
}

fn text<'v>(action: &'v Value, key: &str) -> Result<&'v str, AppError> {
    action
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::operation(format!("needs `{key}`")))
}

fn field<'v>(action: &'v Value, key: &str) -> Result<&'v Value, AppError> {
    action
        .get(key)
        .ok_or_else(|| AppError::operation(format!("needs `{key}`")))
}

impl Press<'_> {
    /// Runs one action: whether it ran, and what it did.
    fn run(
        &mut self,
        action_type: &str,
        action: &Value,
        writer: &mut dyn MetaBindWriter,
    ) -> Result<(bool, String), AppError> {
        let templater = action.get("templater").and_then(Value::as_bool) == Some(true);
        match action_type {
            "updateMetadata" => self.update_metadata(action),
            "createNote" => self.create_note(action),
            "replaceInNote" | "insertIntoNote" | "replaceSelf" if templater => Ok((
                false,
                "`templater: true` runs the text through Templater in Obsidian".to_string(),
            )),
            "replaceInNote" => self.edit_own_note(|source, block| {
                let from =
                    line_number(field(action, "fromLine")?, block).map_err(AppError::operation)?;
                let to =
                    line_number(field(action, "toLine")?, block).map_err(AppError::operation)?;
                replace_lines(source, from, to, text(action, "replacement")?)
                    .map(|updated| (updated, format!("replaced lines {from} to {to}")))
                    .map_err(AppError::operation)
            }),
            "insertIntoNote" => self.edit_own_note(|source, block| {
                let line =
                    line_number(field(action, "line")?, block).map_err(AppError::operation)?;
                insert_at_line(source, line, text(action, "value")?)
                    .map(|updated| (updated, format!("inserted at line {line}")))
                    .map_err(AppError::operation)
            }),
            "regexpReplaceInNote" => self.edit_own_note(|source, _| {
                let pattern = text(action, "regexp")?;
                let flags = action.get("regexpFlags").and_then(Value::as_str);
                regexp_replace(source, pattern, flags, text(action, "replacement")?)
                    .map(|updated| (updated, format!("replaced matches of `{pattern}`")))
                    .map_err(AppError::operation)
            }),
            "replaceSelf" => self.edit_own_note(|source, block| {
                let block = block.ok_or_else(|| {
                    AppError::operation("only a button declared in the note can replace itself")
                })?;
                replace_block(source, block, text(action, "replacement")?)
                    .map(|updated| (updated, "replaced the button".to_string()))
                    .map_err(AppError::operation)
            }),
            "sleep" => Ok((true, "no wait outside Obsidian".to_string())),
            "js" | "inlineJS" => self.run_js(action_type, action, writer),
            "command" | "open" | "input" | "runTemplaterFile" | "templaterCreateNote" => {
                Ok((false, "runs in Obsidian".to_string()))
            }
            other => Ok((false, format!("unknown action type `{other}`"))),
        }
    }

    fn current(&self) -> &str {
        &self.sources[self.path].1
    }

    /// Finds the button's block in the note as it now is.
    fn block(&self) -> Result<Option<ButtonBlock>, AppError> {
        let Some(text) = &self.block_text else {
            return Ok(None);
        };
        let source = self.current();
        let mut found = source.match_indices(text.as_str());
        let (Some((start, _)), None) = (found.next(), found.next()) else {
            return Err(AppError::operation(
                "an earlier action changed the button's block, so it cannot be found again",
            ));
        };
        let line = |offset: usize| source[..offset].matches('\n').count() + 1;
        let end = start + text.len();
        Ok(Some(ButtonBlock {
            byte_range: start..end,
            first_line: line(start),
            last_line: line(source[..end].trim_end_matches(['\n', '\r']).len()),
        }))
    }

    fn edit_own_note(
        &mut self,
        edit: impl FnOnce(&str, Option<&ButtonBlock>) -> Result<(String, String), AppError>,
    ) -> Result<(bool, String), AppError> {
        if let Some(guard) = &self.scope.guard {
            vulcan_core::PermissionGuard::check_write_path(guard, self.path)
                .map_err(AppError::operation)?;
        }
        let block = self.block()?;
        let (updated, detail) = edit(self.current(), block.as_ref())?;
        if !self.sources.contains_key(self.path) {
            let source = std::fs::read_to_string(self.paths.vault_root().join(self.path))
                .map_err(AppError::operation)?;
            self.sources
                .insert(self.path.to_string(), (source.clone(), source));
        }
        self.sources.get_mut(self.path).expect("loaded").1 = updated;
        Ok((true, detail))
    }

    fn update_metadata(&mut self, action: &Value) -> Result<(bool, String), AppError> {
        let target = BindTarget::parse(text(action, "bindTarget")?).map_err(AppError::operation)?;
        let value = field(action, "value")?;
        let value = if action.get("evaluate").and_then(Value::as_bool) == Some(true) {
            if !self.config.metabind.enable_js {
                return Err(AppError::operation(
                    "`evaluate: true` runs JavaScript, which needs `metabind.enable_js`",
                ));
            }
            let expression = value
                .as_str()
                .map_or_else(|| value.to_string(), ToOwned::to_owned);
            self.evaluate(&target, &expression)?
        } else {
            value.clone()
        };
        let before = self.report.changes.len();
        let mut sources = std::mem::take(&mut self.sources);
        let mut plan = super::MetaBindWritePlan::default();
        let result = write_value(
            self.paths,
            &self.scope,
            self.path,
            &sources[self.path].1.clone(),
            &target,
            value.clone(),
            &mut sources,
            &mut plan,
        );
        self.sources = sources;
        result?;
        self.report.changes.extend(plan.changes);
        Ok((
            true,
            if self.report.changes.len() > before {
                format!("set `{target}` to {value}")
            } else {
                format!("`{target}` is already {value}")
            },
        ))
    }

    /// Reads `target` from the notes as they now are.
    fn read_target(&self, target: &BindTarget) -> Result<Value, AppError> {
        let file = match &target.file {
            None => self.path.to_string(),
            Some(file) => {
                let index = self.scope.note_index.as_ref().ok_or_else(|| {
                    AppError::operation("the vault is not indexed; run `vulcan scan`")
                })?;
                resolve_target_note(index, self.path, file)
                    .map(|note| note.document_path.clone())
                    .ok_or_else(|| AppError::operation(format!("no note `{file}`")))?
            }
        };
        let source = if let Some((_, current)) = self.sources.get(&file) {
            current.clone()
        } else {
            if let Some(guard) = &self.scope.guard {
                vulcan_core::PermissionGuard::check_read_path(guard, &file)
                    .map_err(AppError::operation)?;
            }
            std::fs::read_to_string(self.paths.vault_root().join(&file))
                .map_err(AppError::operation)?
        };
        let parsed = vulcan_core::parse_document(&source, &VaultConfig::default());
        Ok(read_path(&frontmatter_json(&parsed), &target.path)
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Evaluates an `updateMetadata` expression with `x` (the target's
    /// value) and `getMetadata(target)` for the targets the expression
    /// names as string literals, in the strict sandbox.
    fn evaluate(&self, target: &BindTarget, expression: &str) -> Result<Value, AppError> {
        let x = self.read_target(target)?;
        let mut metadata = serde_json::Map::new();
        let pattern = regex::Regex::new(r#"getMetadata\(\s*(?:'([^']*)'|"([^"]*)")\s*\)"#)
            .expect("valid pattern");
        for captures in pattern.captures_iter(expression) {
            let name = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map_or("", |name| name.as_str());
            let named = BindTarget::parse(name).map_err(AppError::operation)?;
            metadata.insert(name.to_string(), self.read_target(&named)?);
        }
        let session = DataviewJsSession::new(
            self.paths,
            Some(self.path),
            DataviewJsEvalOptions {
                sandbox: Some(vulcan_core::config::JsRuntimeSandbox::Strict),
                read_only: true,
                permission_profile: self.options.permission_profile.map(ToOwned::to_owned),
                ..DataviewJsEvalOptions::default()
            },
        )
        .map_err(AppError::operation)?;
        let body = format!(
            "const x = {x};\nconst __metadata = {};\n\
             const getMetadata = (target) => {{ \
             if (!(target in __metadata)) throw new Error(`getMetadata takes a target written as a string literal, not ${{target}}`); \
             return __metadata[target]; }};\n\
             return ({expression}\n);",
            Value::Object(metadata)
        );
        Ok(session
            .evaluate_async_function(&body)
            .map_err(AppError::operation)?
            .unwrap_or(Value::Null))
    }

    fn create_note(&mut self, action: &Value) -> Result<(bool, String), AppError> {
        let folder = action
            .get("folderPath")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_matches('/');
        let name = action
            .get("fileName")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Untitled");
        let name = name.strip_suffix(".md").unwrap_or(name);
        let note_path = |suffix: &str| {
            let file = format!("{name}{suffix}.md");
            if folder.is_empty() {
                file
            } else {
                format!("{folder}/{file}")
            }
        };
        let exists = |path: &str| {
            self.sources.contains_key(path) || self.paths.vault_root().join(path).exists()
        };
        let mut path = note_path("");
        if exists(&path) {
            if action.get("openIfAlreadyExists").and_then(Value::as_bool) == Some(true) {
                return Ok((true, format!("{path} already exists")));
            }
            let mut counter = 1;
            loop {
                path = note_path(&format!(" {counter}"));
                if !exists(&path) {
                    break;
                }
                counter += 1;
            }
        }
        let path = vulcan_core::paths::normalize_relative_input_path(&path, PLAIN_PATH)
            .map_err(AppError::operation)?;
        if let Some(guard) = &self.scope.guard {
            vulcan_core::PermissionGuard::check_write_path(guard, &path)
                .map_err(AppError::operation)?;
        }
        self.sources
            .insert(path.clone(), (String::new(), String::new()));
        self.created.insert(path.clone());
        let opened = if action.get("openNote").and_then(Value::as_bool) == Some(true) {
            " (open it in your editor)"
        } else {
            ""
        };
        Ok((true, format!("created {path}{opened}")))
    }

    fn run_js(
        &mut self,
        action_type: &str,
        action: &Value,
        writer: &mut dyn MetaBindWriter,
    ) -> Result<(bool, String), AppError> {
        if !self.config.metabind.enable_js {
            return Err(AppError::operation(
                "JavaScript actions need `metabind.enable_js`",
            ));
        }
        crate::plugins::require_trusted_plugin_execution(self.paths, Some("Meta Bind JavaScript"))?;
        let (code, js_file) = if action_type == "js" {
            let file = text(action, "file")?.trim_start_matches('/');
            let relative = vulcan_core::paths::normalize_relative_input_path(file, PLAIN_PATH)
                .map_err(AppError::operation)?;
            if let Some(guard) = &self.scope.guard {
                vulcan_core::PermissionGuard::check_read_path(guard, &relative)
                    .map_err(AppError::operation)?;
            }
            let code = std::fs::read_to_string(self.paths.vault_root().join(&relative))
                .map_err(|error| AppError::operation(format!("{relative}: {error}")))?;
            (code, Some(relative))
        } else {
            (text(action, "code")?.to_string(), None)
        };
        // The script sees the edits of the actions before it.
        self.flush(writer)?;
        let session = super::super::customjs::invocation_session(
            self.paths,
            self.options.permission_profile,
            self.options.dry_run,
        )?;
        let block = self.block().ok().flatten();
        let context = serde_json::json!({
            "args": action.get("args").cloned().unwrap_or(Value::Object(serde_json::Map::new())),
            "file": { "path": self.path },
            "jsFile": js_file.as_ref().map(|path| serde_json::json!({ "path": path })),
            "buttonConfig": self.button.config,
            "buttonContext": {
                "position": block.map(|block| serde_json::json!({
                    "lineStart": block.first_line - 1,
                    "lineEnd": block.last_line - 1,
                })),
                "isInGroup": false,
                "isInline": self.button.block.is_none(),
            },
        });
        let body = format!(
            "const context = {context};\n\
             const __notices = [];\n\
             class Notice {{ constructor(message) {{ __notices.push(String(message)); }} }}\n\
             await (async () => {{\n{code}\n}})();\n\
             return __notices;"
        );
        let notices = session
            .evaluate_async_function(&body)
            .map_err(AppError::operation)?;
        let notices =
            notices
                .as_ref()
                .and_then(Value::as_array)
                .map_or_else(String::new, |notices| {
                    notices
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                });
        let ran = js_file.map_or_else(
            || "ran inline JavaScript".to_string(),
            |file| format!("ran {file}"),
        );
        Ok((
            true,
            if notices.is_empty() {
                ran
            } else {
                format!("{ran}: {notices}")
            },
        ))
    }

    /// Writes the edits so far (for a dry run, only records them) and
    /// forgets the notes, so later actions read what scripts wrote.
    fn flush(&mut self, writer: &mut dyn MetaBindWriter) -> Result<(), AppError> {
        let sources = std::mem::take(&mut self.sources);
        let mut kept = BTreeMap::new();
        for (path, (before, after)) in sources {
            let created = self.created.contains(&path);
            if before == after && !created {
                if self.options.dry_run {
                    kept.insert(path, (before, after));
                }
                continue;
            }
            let write = MetaBindFileWrite {
                path: path.clone(),
                before: before.clone(),
                after: after.clone(),
                created,
            };
            if self.options.dry_run {
                kept.insert(path, (before, after));
            } else {
                writer.write(&write)?;
                self.created.remove(&path);
                self.writes.push(write);
            }
        }
        if self.options.dry_run {
            self.sources = kept;
            self.finish_dry_run();
            return Ok(());
        }
        let source = std::fs::read_to_string(self.paths.vault_root().join(self.path))
            .map_err(AppError::operation)?;
        self.sources
            .insert(self.path.to_string(), (source.clone(), source));
        Ok(())
    }

    /// A dry run keeps every edit in memory; its writes are what differs.
    fn finish_dry_run(&mut self) {
        self.writes = self
            .sources
            .iter()
            .filter(|(path, (before, after))| before != after || self.created.contains(*path))
            .map(|(path, (before, after))| MetaBindFileWrite {
                path: path.clone(),
                before: before.clone(),
                after: after.clone(),
                created: self.created.contains(path),
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes to disk, as the CLI's writer does without hooks and refresh.
    struct DiskWriter<'a>(&'a VaultPaths, Vec<String>);

    impl MetaBindWriter for DiskWriter<'_> {
        fn write(&mut self, write: &MetaBindFileWrite) -> Result<(), AppError> {
            let target = self.0.vault_root().join(&write.path);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("dir");
            std::fs::write(target, &write.after).expect("write");
            self.1.push(write.path.clone());
            Ok(())
        }
    }

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
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).expect("scan");
        (dir, paths)
    }

    fn press(
        paths: &VaultPaths,
        path: &str,
        selector: &ButtonSelector,
        dry_run: bool,
    ) -> Result<(MetaBindPressReport, Vec<String>), AppError> {
        let config = vulcan_core::load_vault_config(paths).config;
        let source = std::fs::read_to_string(paths.vault_root().join(path)).expect("read");
        let mut writer = DiskWriter(paths, Vec::new());
        let report = press_metabind_button(
            paths,
            &config,
            path,
            &source,
            selector,
            MetaBindPressOptions {
                dry_run,
                permission_profile: None,
            },
            &mut writer,
        )?;
        Ok((report, writer.1))
    }

    const TEXT_BUTTON: &str = "---\ncount: 1\n---\nInsert below:\nold\n\n```meta-bind-button\nlabel: Edit\nid: edit\nactions:\n  - type: updateMetadata\n    bindTarget: count\n    value: 5\n  - type: insertIntoNote\n    line: selfStart - 2\n    value: inserted\n  - type: regexpReplaceInNote\n    regexp: \"old\\\\n\"\n    replacement: \"\"\n  - type: command\n    command: workspace:new-tab\n  - type: replaceSelf\n    replacement: done\n```\n";

    #[test]
    fn text_actions_edit_the_note_in_order() {
        let (_dir, paths) = vault("", &[("Note.md", TEXT_BUTTON)]);
        let (dry, written) = press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("edit".to_string()),
            true,
        )
        .expect("dry");
        assert!(written.is_empty(), "a dry run writes nothing");
        assert_eq!(dry.written, vec!["Note.md".to_string()]);
        assert_eq!(
            std::fs::read_to_string(paths.vault_root().join("Note.md")).expect("read"),
            TEXT_BUTTON
        );

        let (report, written) =
            press(&paths, "Note.md", &ButtonSelector::Line(8), false).expect("press");
        assert_eq!(written, vec!["Note.md".to_string()]);
        assert_eq!(report.label, "Edit");
        assert_eq!(report.changes[0].after, Value::from(5));
        assert!(!report.actions[3].ran, "{:?}", report.actions[3]);
        assert_eq!(
            std::fs::read_to_string(paths.vault_root().join("Note.md")).expect("read"),
            "---\ncount: 5\n---\nInsert below:\ninserted\n\ndone\n"
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn evaluated_updates_need_enable_js_and_see_other_values() {
        let note = "---\ncount: 2\nstep: 3\n---\n```meta-bind-button\nlabel: Add\nid: add\naction:\n  type: updateMetadata\n  bindTarget: count\n  evaluate: true\n  value: \"x + getMetadata('step') + getMetadata('Other#bonus')\"\n```\n";
        let other = "---\nbonus: 10\n---\n";
        let (_dir, paths) = vault("", &[("Note.md", note), ("Other.md", other)]);
        let error = press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("add".to_string()),
            false,
        )
        .expect_err("needs enable_js");
        assert!(error.to_string().contains("metabind.enable_js"), "{error}");

        std::fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            "[metabind]\nenable_js = true\n",
        )
        .expect("config");
        press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("add".to_string()),
            false,
        )
        .expect("press");
        let updated = std::fs::read_to_string(paths.vault_root().join("Note.md")).expect("read");
        assert!(updated.contains("count: 15"), "{updated}");
    }

    #[test]
    fn create_note_numbers_names_that_are_taken() {
        let note = "```meta-bind-button\nlabel: New\nid: new\nactions:\n  - type: createNote\n    folderPath: Inbox\n    fileName: Idea\n  - type: createNote\n    folderPath: Inbox\n    fileName: Idea\n```\n";
        let (_dir, paths) = vault("", &[("Note.md", note), ("Inbox/Idea.md", "taken\n")]);
        let (report, _) = press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("new".to_string()),
            false,
        )
        .expect("press");
        assert_eq!(
            report.created,
            vec!["Inbox/Idea 1.md".to_string(), "Inbox/Idea 2.md".to_string()]
        );
        assert!(paths.vault_root().join("Inbox/Idea 2.md").exists());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn js_actions_need_trust_and_see_earlier_edits() {
        let _trust = crate::trust::IsolatedTrust::new();
        let note = "---\ncount: 1\n---\n```meta-bind-button\nlabel: Run\nid: run\nactions:\n  - type: updateMetadata\n    bindTarget: count\n    value: 7\n  - type: js\n    file: scripts/hello.js\n    args:\n      greeting: you\n```\n";
        let script = "const page = dv.page(context.file.path);\nnew Notice(`Hello ${context.args.greeting} from ${context.jsFile.path}`);\n";
        let (_dir, paths) = vault(
            "[metabind]\nenable_js = true\n",
            &[("Note.md", note), ("scripts/hello.js", script)],
        );
        let error = press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("run".to_string()),
            false,
        )
        .expect_err("untrusted");
        assert!(error.to_string().contains("trusted vault"), "{error}");

        crate::trust::add_trust(paths.vault_root()).expect("trust");
        let (report, written) = press(
            &paths,
            "Note.md",
            &ButtonSelector::Id("run".to_string()),
            false,
        )
        .expect("press");
        assert_eq!(
            written,
            vec!["Note.md".to_string()],
            "flushed before the script"
        );
        assert_eq!(
            report.actions[1].detail,
            "ran scripts/hello.js: Hello you from scripts/hello.js"
        );
    }
}
