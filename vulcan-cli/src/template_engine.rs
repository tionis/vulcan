use super::{
    parse_frontmatter_document, render_template_contents, resolve_template_file,
    template_variables_for_path, CliError, TemplateCandidate, TemplateTimestamp, YamlMapping,
    YamlValue,
};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use vulcan_core::config::TemplatesConfig;
use vulcan_core::expression::functions::{format_date, parse_date_with_format, parse_date_like_string};
use vulcan_core::move_note;
use vulcan_core::parser::parse_document;
use vulcan_core::{resolve_note_reference, VaultConfig, VaultPaths};

const MAX_TEMPLATE_INCLUDE_DEPTH: usize = 10;
const DEFAULT_FILE_DATE_FORMAT: &str = "YYYY-MM-DD HH:mm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateEngineKind {
    Auto,
    Native,
    Templater,
}

impl TemplateEngineKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Templater => "templater",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateRunMode {
    Create,
    Append,
    Dynamic,
}

impl TemplateRunMode {
    fn config_code(self) -> i64 {
        match self {
            Self::Create => 0,
            Self::Append => 1,
            Self::Dynamic => 5,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TemplateRenderRequest<'a> {
    pub(crate) paths: &'a VaultPaths,
    pub(crate) vault_config: &'a VaultConfig,
    pub(crate) templates: &'a [TemplateCandidate],
    pub(crate) template_path: Option<&'a Path>,
    pub(crate) template_text: &'a str,
    pub(crate) target_path: &'a str,
    pub(crate) target_contents: Option<&'a str>,
    pub(crate) engine: TemplateEngineKind,
    pub(crate) vars: &'a HashMap<String, String>,
    pub(crate) allow_mutations: bool,
    pub(crate) run_mode: TemplateRunMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateRenderOutput {
    pub(crate) content: String,
    pub(crate) target_path: String,
    pub(crate) engine: TemplateEngineKind,
    pub(crate) warnings: Vec<String>,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) changed_paths: Vec<String>,
}

pub(crate) fn parse_template_var_bindings(
    vars: &[String],
) -> Result<HashMap<String, String>, CliError> {
    let mut parsed = HashMap::new();
    for binding in vars {
        let Some((key, value)) = binding.split_once('=') else {
            return Err(CliError::operation(format!(
                "template variable bindings must use key=value syntax: {binding}"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(CliError::operation(format!(
                "template variable bindings need a non-empty key: {binding}"
            )));
        }
        parsed.insert(key.to_string(), value.to_string());
    }
    Ok(parsed)
}

pub(crate) fn detect_template_engine(
    source: &str,
    requested: TemplateEngineKind,
) -> TemplateEngineKind {
    match requested {
        TemplateEngineKind::Auto => {
            if source.contains("<%") {
                TemplateEngineKind::Templater
            } else {
                TemplateEngineKind::Native
            }
        }
        explicit => explicit,
    }
}

pub(crate) fn render_template_request(
    request: TemplateRenderRequest<'_>,
) -> Result<TemplateRenderOutput, CliError> {
    let engine = detect_template_engine(request.template_text, request.engine);
    let template_text = request.template_text;
    let mut session = TemplateSession::new(request, engine);
    let content = session.render_source(template_text, engine, 0)?;
    let content = session.merge_pending_frontmatter(&content)?;
    #[cfg(feature = "js_runtime")]
    let content = session.run_post_render_hooks(content)?;
    Ok(TemplateRenderOutput {
        content,
        target_path: session.target_path,
        engine,
        warnings: session.warnings,
        diagnostics: session.diagnostics,
        changed_paths: session.changed_paths.into_iter().collect(),
    })
}

struct TemplateSession<'a> {
    request: TemplateRenderRequest<'a>,
    target_path: String,
    target_contents: String,
    timestamp: TemplateTimestamp,
    warnings: Vec<String>,
    diagnostics: Vec<String>,
    changed_paths: BTreeSet<String>,
    pending_frontmatter: Option<YamlMapping>,
    prompt_count: usize,
    suggester_count: usize,
    #[cfg(feature = "js_runtime")]
    js_runtime: Option<JsTemplateRuntime>,
}

impl<'a> TemplateSession<'a> {
    fn new(request: TemplateRenderRequest<'a>, _engine: TemplateEngineKind) -> Self {
        Self {
            target_path: request.target_path.to_string(),
            target_contents: request.target_contents.unwrap_or_default().to_string(),
            timestamp: TemplateTimestamp::current(),
            request,
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            changed_paths: BTreeSet::new(),
            pending_frontmatter: None,
            prompt_count: 0,
            suggester_count: 0,
            #[cfg(feature = "js_runtime")]
            js_runtime: None,
        }
    }

    fn templates_config(&self) -> &TemplatesConfig {
        &self.request.vault_config.templates
    }

    fn builtins_for_path(&self, path: &str) -> super::TemplateVariables {
        template_variables_for_path(path, self.timestamp)
    }

    fn current_builtins(&self) -> super::TemplateVariables {
        self.builtins_for_path(&self.target_path)
    }

    fn render_source(
        &mut self,
        template: &str,
        engine: TemplateEngineKind,
        include_depth: usize,
    ) -> Result<String, CliError> {
        if include_depth > MAX_TEMPLATE_INCLUDE_DEPTH {
            return Err(CliError::operation(format!(
                "Reached inclusion depth limit (max = {MAX_TEMPLATE_INCLUDE_DEPTH})"
            )));
        }

        if engine == TemplateEngineKind::Native {
            return Ok(render_template_contents(
                template,
                &self.current_builtins(),
                self.templates_config(),
            ));
        }

        let mut output = String::with_capacity(template.len());
        let mut cursor = 0_usize;
        while let Some(start) = template[cursor..].find("<%") {
            let absolute_start = cursor + start;
            let text_segment = &template[cursor..absolute_start];
            output.push_str(&render_template_contents(
                text_segment,
                &self.current_builtins(),
                self.templates_config(),
            ));
            let (tag, next_cursor) = parse_templater_tag(template, absolute_start)
                .ok_or_else(|| CliError::operation("unterminated templater tag"))?;
            apply_left_trim(&mut output, tag.left_trim);
            let replacement = self.evaluate_tag(&tag, include_depth)?;
            output.push_str(&replacement);
            cursor = apply_right_trim(template, next_cursor, tag.right_trim);
        }

        let tail = &template[cursor..];
        output.push_str(&render_template_contents(
            tail,
            &self.current_builtins(),
            self.templates_config(),
        ));
        Ok(output)
    }

    fn evaluate_tag(
        &mut self,
        tag: &TemplaterTag<'_>,
        include_depth: usize,
    ) -> Result<String, CliError> {
        if tag.dynamic {
            self.push_diagnostic(
                "Templater dynamic tags (`<%+ %>`) are evaluated eagerly in CLI templates"
                    .to_string(),
            );
        }

        if tag.execution {
            return self.evaluate_code(&tag.body);
        }

        match self.evaluate_native_expression(&tag.body, include_depth) {
            Ok(value) => Ok(template_value_to_string(&value)),
            Err(NativeExpressionError::RequiresJsRuntime(message)) => {
                #[cfg(feature = "js_runtime")]
                {
                    let _ = &message;
                    self.evaluate_js_expression(&tag.body)
                }
                #[cfg(not(feature = "js_runtime"))]
                {
                    self.push_diagnostic(message);
                    Ok(String::new())
                }
            }
            Err(NativeExpressionError::Message(message)) => Err(CliError::operation(message)),
        }
    }

    fn evaluate_code(&mut self, _source: &str) -> Result<String, CliError> {
        #[cfg(feature = "js_runtime")]
        {
            return self.evaluate_js_code(_source);
        }
        #[cfg(not(feature = "js_runtime"))]
        {
            self.push_diagnostic(
                "Templater execution tags (`<%* %>`) require the `js_runtime` feature"
                    .to_string(),
            );
            Ok(String::new())
        }
    }

    fn evaluate_native_expression(
        &mut self,
        source: &str,
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let expression = parse_native_expression(source)?;
        self.eval_native_expression(&expression, include_depth)
    }

    fn eval_native_expression(
        &mut self,
        expression: &NativeExpression,
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        match expression {
            NativeExpression::String(value) => Ok(TemplateValue::String(value.clone())),
            NativeExpression::Number(value) => Ok(TemplateValue::Number(*value)),
            NativeExpression::Bool(value) => Ok(TemplateValue::Bool(*value)),
            NativeExpression::Null => Ok(TemplateValue::Null),
            NativeExpression::Path(path) => self.eval_native_path(path),
            NativeExpression::Call { callee, args } => {
                self.eval_native_call(callee, args, include_depth)
            }
        }
    }

    fn eval_native_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(first) = path.first() else {
            return Ok(TemplateValue::Null);
        };
        let NativePathPart::Name(root) = first else {
            return Err(NativeExpressionError::Message(
                "templater expressions must start with an identifier".to_string(),
            ));
        };
        if root == "tp" {
            return self.resolve_tp_path(path);
        }

        if path.len() == 1 {
            return Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater expression `{root}` requires the `js_runtime` feature"
            )));
        }

        Err(NativeExpressionError::RequiresJsRuntime(
            "templater expression requires the `js_runtime` feature".to_string(),
        ))
    }

    fn resolve_tp_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if path.len() < 2 {
            return Ok(TemplateValue::Null);
        }

        match path_name(path, 1) {
            Some("file") => self.resolve_tp_file_path(path),
            Some("frontmatter") => self.resolve_tp_frontmatter_path(path),
            Some("config") => self.resolve_tp_config_path(path),
            Some("obsidian") => Err(NativeExpressionError::RequiresJsRuntime(
                "tp.obsidian helpers require the `js_runtime` feature".to_string(),
            )),
            Some("app") => {
                self.push_diagnostic("tp.app.* is not available in CLI templates".to_string());
                Ok(TemplateValue::Null)
            }
            Some("user") | Some("web") => Err(NativeExpressionError::RequiresJsRuntime(
                format!(
                    "templater path `{}` requires the `js_runtime` feature",
                    join_native_path(path)
                ),
            )),
            Some("date") | Some("system") => Err(NativeExpressionError::RequiresJsRuntime(
                format!(
                    "templater function `{}` must be called, not accessed as a value",
                    join_native_path(path)
                ),
            )),
            _ => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater path `{}` requires the `js_runtime` feature",
                join_native_path(path)
            ))),
        }
    }

    fn resolve_tp_file_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if path.len() < 3 {
            return Ok(TemplateValue::Object(self.current_file_object()));
        }
        match path_name(path, 2) {
            Some("title") => Ok(TemplateValue::String(self.current_title())),
            Some("content") => Ok(TemplateValue::String(self.target_contents.clone())),
            Some("tags") => Ok(TemplateValue::Array(
                self.current_tags()
                    .into_iter()
                    .map(TemplateValue::String)
                    .collect(),
            )),
            Some("path") if path.len() == 3 => Ok(TemplateValue::String(self.target_path.clone())),
            Some("folder") if path.len() == 3 => Ok(TemplateValue::String(self.relative_folder())),
            Some(name) => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater path `tp.file.{name}` requires the `js_runtime` feature"
            ))),
            None => Ok(TemplateValue::Null),
        }
    }

    fn resolve_tp_frontmatter_path(
        &self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let frontmatter = current_frontmatter_mapping(&self.target_contents).unwrap_or_default();
        let mut value = JsonValue::Object(yaml_mapping_to_json_object(&frontmatter));

        for part in path.iter().skip(2) {
            let key = match part {
                NativePathPart::Name(name) | NativePathPart::Index(name) => name,
            };
            value = value
                .as_object()
                .and_then(|object| object.get(key))
                .cloned()
                .unwrap_or(JsonValue::Null);
        }

        Ok(TemplateValue::from_json(value))
    }

    fn resolve_tp_config_path(
        &self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let config = self.config_json_value();
        let mut value = config;
        for part in path.iter().skip(2) {
            let key = match part {
                NativePathPart::Name(name) | NativePathPart::Index(name) => name,
            };
            value = value
                .as_object()
                .and_then(|object| object.get(key))
                .cloned()
                .unwrap_or(JsonValue::Null);
        }
        Ok(TemplateValue::from_json(value))
    }

    fn eval_native_call(
        &mut self,
        callee: &[NativePathPart],
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let signature = join_native_path(callee);
        match signature.as_str() {
            "tp.date.now" => self.tp_date_now(args, false),
            "tp.date.tomorrow" => self.tp_date_fixed_offset(args, 1),
            "tp.date.yesterday" => self.tp_date_fixed_offset(args, -1),
            "tp.date.weekday" => self.tp_date_weekday(args),
            "tp.file.path" => self.tp_file_path_call(args),
            "tp.file.folder" => self.tp_file_folder_call(args),
            "tp.file.creation_date" => self.tp_file_timestamp_call(args, true),
            "tp.file.last_modified_date" => self.tp_file_timestamp_call(args, false),
            "tp.file.exists" => self.tp_file_exists_call(args),
            "tp.file.include" => self.tp_file_include_call(args, include_depth),
            "tp.file.create_new" => self.tp_file_create_new_call(args),
            "tp.file.move" => self.tp_file_move_call(args),
            "tp.file.rename" => self.tp_file_rename_call(args),
            "tp.file.cursor" => Ok(TemplateValue::String(String::new())),
            "tp.file.find_tfile" => self.tp_file_find_tfile_call(args),
            "tp.system.prompt" => self.tp_system_prompt_call(args),
            "tp.system.suggester" => self.tp_system_suggester_call(args),
            "tp.system.clipboard" => Ok(TemplateValue::String(read_clipboard_best_effort())),
            signature if signature.starts_with("tp.web.")
                || signature.starts_with("tp.user.")
                || signature.starts_with("tp.obsidian.") =>
            {
                Err(NativeExpressionError::RequiresJsRuntime(format!(
                    "templater function `{signature}` requires the `js_runtime` feature"
                )))
            }
            _ => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater function `{signature}` requires the `js_runtime` feature"
            ))),
        }
    }

    fn tp_date_now(
        &self,
        args: &[NativeExpression],
        weekday_mode: bool,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let offset = args.get(1);
        let reference = string_arg(args, 2);
        let reference_format = string_arg(args, 3);
        let mut millis = self.reference_timestamp(reference.as_deref(), reference_format.as_deref())?;
        if !weekday_mode {
            millis = apply_date_offset(millis, offset)?;
        }
        Ok(TemplateValue::String(format_date(millis, &format)))
    }

    fn tp_date_fixed_offset(
        &self,
        args: &[NativeExpression],
        day_offset: i64,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let millis = self.reference_timestamp(None, None)? + day_offset * 86_400_000;
        Ok(TemplateValue::String(format_date(millis, &format)))
    }

    fn tp_date_weekday(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let weekday = number_arg(args, 1).ok_or_else(|| {
            NativeExpressionError::Message("tp.date.weekday requires a weekday number".to_string())
        })?;
        let reference = string_arg(args, 2);
        let reference_format = string_arg(args, 3);
        let millis = self.reference_timestamp(reference.as_deref(), reference_format.as_deref())?;
        let target = weekday_timestamp(millis, weekday);
        Ok(TemplateValue::String(format_date(target, &format)))
    }

    fn tp_file_path_call(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let absolute = bool_arg(args, 0).unwrap_or(false);
        Ok(TemplateValue::String(if absolute {
            self.request
                .paths
                .vault_root()
                .join(&self.target_path)
                .display()
                .to_string()
        } else {
            self.target_path.clone()
        }))
    }

    fn tp_file_folder_call(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let absolute = bool_arg(args, 0).unwrap_or(false);
        Ok(TemplateValue::String(if absolute {
            self.absolute_folder()
        } else {
            self.relative_folder()
        }))
    }

    fn tp_file_timestamp_call(
        &self,
        args: &[NativeExpression],
        creation: bool,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format =
            string_arg(args, 0).unwrap_or_else(|| DEFAULT_FILE_DATE_FORMAT.to_string());
        let absolute = self.request.paths.vault_root().join(&self.target_path);
        let timestamp = if absolute.is_file() {
            file_timestamp_millis(&absolute, creation)
        } else {
            Some(self.current_timestamp_millis())
        }
        .unwrap_or_else(|| self.current_timestamp_millis());
        Ok(TemplateValue::String(format_date(timestamp, &format)))
    }

    fn tp_file_exists_call(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(path) = string_arg(args, 0) else {
            return Ok(TemplateValue::Bool(false));
        };
        let exists = self.resolve_vault_path(&path).is_some();
        Ok(TemplateValue::Bool(exists))
    }

    fn tp_file_include_call(
        &mut self,
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(path) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        let include_path = self.resolve_include_target(&path)?;
        let source = fs::read_to_string(self.request.paths.vault_root().join(&include_path))
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        let rendered = self
            .render_source(&source, TemplateEngineKind::Templater, include_depth + 1)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        self.capture_frontmatter_from_rendered_include(&rendered)?;
        let (_, body) = parse_frontmatter_document(&rendered, true)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        Ok(TemplateValue::String(body))
    }

    fn tp_file_create_new_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if !self.request.allow_mutations {
            self.push_diagnostic(
                "tp.file.create_new() is disabled during template preview".to_string(),
            );
            return Ok(TemplateValue::Null);
        }

        let Some(template_arg) = args.first() else {
            return Ok(TemplateValue::Null);
        };
        let filename = string_arg(args, 1).unwrap_or_else(|| "Untitled".to_string());
        let folder = string_arg(args, 3).unwrap_or_default();
        let relative_path = if folder.trim().is_empty() {
            format!("{filename}.md")
        } else {
            format!("{}/{}.md", folder.trim_matches('/'), filename)
        };
        let normalized = normalize_note_output_path(&relative_path)
            .map_err(NativeExpressionError::Message)?;
        let content = match self.eval_native_expression(template_arg, 0)? {
            TemplateValue::String(value) => match resolve_template_file(
                self.request.paths,
                self.request.templates,
                &value,
            ) {
                Ok(template) => {
                    let source = fs::read_to_string(&template.absolute_path)
                        .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
                    self.render_source(&source, TemplateEngineKind::Templater, 0)
                        .map_err(|error| NativeExpressionError::Message(error.to_string()))?
                }
                Err(_) => value,
            },
            other => template_value_to_string(&other),
        };
        let absolute = self.request.paths.vault_root().join(&normalized);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        }
        fs::write(&absolute, content)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        self.changed_paths.insert(normalized.clone());
        Ok(TemplateValue::Object(file_object_json(&normalized)))
    }

    fn tp_file_move_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(new_path) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        let normalized =
            normalize_note_output_path(&new_path).map_err(NativeExpressionError::Message)?;
        if !self.request.allow_mutations {
            self.push_diagnostic("tp.file.move() is disabled during template preview".to_string());
            return Ok(TemplateValue::Null);
        }

        if self.request.paths.vault_root().join(&self.target_path).is_file() {
            let summary = move_note(self.request.paths, &self.target_path, &normalized, false)
                .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
            self.record_move_summary(&summary);
        }
        self.target_path = normalized;
        Ok(TemplateValue::String(String::new()))
    }

    fn tp_file_rename_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(new_name) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        if new_name.contains(['\\', '/', ':']) {
            return Err(NativeExpressionError::Message(
                "File name cannot contain any of these characters: \\ / :".to_string(),
            ));
        }
        let folder = self.relative_folder();
        let path = if folder.is_empty() {
            format!("{new_name}.md")
        } else {
            format!("{folder}/{new_name}.md")
        };
        self.tp_file_move_call(&[NativeExpression::String(path)])
    }

    fn tp_file_find_tfile_call(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(identifier) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        let Some(path) = self.resolve_vault_path(&identifier) else {
            return Ok(TemplateValue::Null);
        };
        Ok(TemplateValue::Object(file_object_json(&path)))
    }

    fn tp_system_prompt_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        self.prompt_count += 1;
        let prompt = string_arg(args, 0).unwrap_or_default();
        let default = string_arg(args, 1);
        let throw_on_cancel = bool_arg(args, 2).unwrap_or(false);
        let key = prompt_lookup_key(&prompt, self.prompt_count);
        if let Some(value) = lookup_template_var(self.request.vars, &key, Some(&prompt)) {
            return Ok(TemplateValue::String(value));
        }
        if io::stdin().is_terminal() {
            let value = read_prompt_value(&prompt, default.as_deref())
                .map_err(NativeExpressionError::Message)?;
            return Ok(match value {
                Some(value) => TemplateValue::String(value),
                None if throw_on_cancel => {
                    return Err(NativeExpressionError::Message(
                        "tp.system.prompt() was cancelled".to_string(),
                    ));
                }
                None => TemplateValue::Null,
            });
        }
        Ok(default.map_or_else(
            || {
                if throw_on_cancel {
                    Err(NativeExpressionError::Message(format!(
                        "tp.system.prompt() needs --var {}=<value> in non-interactive mode",
                        key.slug
                    )))
                } else {
                    Ok(TemplateValue::Null)
                }
            },
            |value| Ok(TemplateValue::String(value)),
        )?)
    }

    fn tp_system_suggester_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        self.suggester_count += 1;
        let values = args
            .get(1)
            .map(|expression| self.eval_native_expression(expression, 0))
            .transpose()?
            .unwrap_or(TemplateValue::Array(Vec::new()));
        let items = values.as_string_list();
        let placeholder = string_arg(args, 3).unwrap_or_default();
        let key = prompt_lookup_key(&placeholder, self.suggester_count);
        if let Some(value) = lookup_template_var(self.request.vars, &key, None) {
            return Ok(TemplateValue::String(value));
        }
        if io::stdin().is_terminal() && !items.is_empty() {
            let value =
                read_suggester_value(&placeholder, &items).map_err(NativeExpressionError::Message)?;
            return Ok(value.map_or(TemplateValue::Null, TemplateValue::String));
        }
        items.first().cloned().map_or(Ok(TemplateValue::Null), |value| {
            Ok(TemplateValue::String(value))
        })
    }

    fn record_move_summary(&mut self, summary: &vulcan_core::MoveSummary) {
        self.changed_paths
            .insert(summary.destination_path.clone());
        for rewritten in &summary.rewritten_files {
            self.changed_paths.insert(rewritten.path.clone());
        }
    }

    fn resolve_include_target(&self, include: &str) -> Result<String, NativeExpressionError> {
        let trimmed = include.trim();
        let target = trimmed
            .strip_prefix("[[")
            .and_then(|value| value.strip_suffix("]]"))
            .unwrap_or(trimmed)
            .split('#')
            .next()
            .unwrap_or(trimmed)
            .trim();
        self.resolve_vault_path(target).ok_or_else(|| {
            NativeExpressionError::Message(format!("File {include} doesn't exist"))
        })
    }

    fn resolve_vault_path(&self, identifier: &str) -> Option<String> {
        if let Ok(reference) = resolve_note_reference(self.request.paths, identifier) {
            return Some(reference.path);
        }
        let path = Path::new(identifier);
        let candidate = if path.extension().is_none() {
            PathBuf::from(format!("{identifier}.md"))
        } else {
            path.to_path_buf()
        };
        let absolute = self.request.paths.vault_root().join(&candidate);
        absolute
            .is_file()
            .then(|| candidate.to_string_lossy().replace('\\', "/"))
    }

    fn capture_frontmatter_from_rendered_include(
        &mut self,
        rendered: &str,
    ) -> Result<(), NativeExpressionError> {
        let (frontmatter, _) = parse_frontmatter_document(rendered, true)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        if let Some(frontmatter) = frontmatter {
            self.pending_frontmatter = Some(match self.pending_frontmatter.take() {
                Some(mut existing) => {
                    merge_yaml_mappings(&mut existing, &frontmatter);
                    existing
                }
                None => frontmatter,
            });
        }
        Ok(())
    }

    fn merge_pending_frontmatter(&mut self, rendered: &str) -> Result<String, CliError> {
        let Some(pending) = self.pending_frontmatter.take() else {
            return Ok(rendered.to_string());
        };
        let (frontmatter, body) =
            parse_frontmatter_document(rendered, true).map_err(CliError::operation)?;
        let merged = match frontmatter {
            Some(mut existing) => {
                merge_yaml_mappings(&mut existing, &pending);
                existing
            }
            None => pending,
        };
        let yaml = serde_yaml::to_string(&YamlValue::Mapping(merged))
            .map_err(CliError::operation)?;
        let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
        Ok(format!("---\n{}---\n{}", yaml.trim_end_matches('\n'), body))
    }

    fn current_title(&self) -> String {
        Path::new(&self.target_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_string()
    }

    fn relative_folder(&self) -> String {
        Path::new(&self.target_path)
            .parent()
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }

    fn absolute_folder(&self) -> String {
        let folder = self.request.paths.vault_root().join(self.relative_folder());
        folder.display().to_string()
    }

    fn current_tags(&self) -> Vec<String> {
        let parsed = parse_document(&self.target_contents, self.request.vault_config);
        parsed.tags.iter().map(|tag| tag.tag_text.clone()).collect()
    }

    fn current_file_object(&self) -> JsonMap<String, JsonValue> {
        file_object_json(&self.target_path)
    }

    fn current_timestamp_millis(&self) -> i64 {
        let strings = self.current_builtins();
        parse_date_like_string(&strings.datetime).unwrap_or_default()
    }

    fn reference_timestamp(
        &self,
        reference: Option<&str>,
        reference_format: Option<&str>,
    ) -> Result<i64, NativeExpressionError> {
        if let Some(reference) = reference {
            if let Some(format) = reference_format {
                return parse_date_with_format(reference, format).ok_or_else(|| {
                    NativeExpressionError::Message(format!(
                        "failed to parse reference date `{reference}` with format `{format}`"
                    ))
                });
            }
            return parse_date_like_string(reference).ok_or_else(|| {
                NativeExpressionError::Message(format!("failed to parse reference date `{reference}`"))
            });
        }
        Ok(self.current_timestamp_millis())
    }

    fn config_json_value(&self) -> JsonValue {
        JsonValue::Object(JsonMap::from_iter([
            (
                "template_file".to_string(),
                self.request
                    .template_path
                    .map(path_to_relative_file_json)
                    .map(JsonValue::Object)
                    .unwrap_or(JsonValue::Null),
            ),
            (
                "target_file".to_string(),
                JsonValue::Object(self.current_file_object()),
            ),
            (
                "run_mode".to_string(),
                JsonValue::Number(self.request.run_mode.config_code().into()),
            ),
            (
                "active_file".to_string(),
                JsonValue::Object(self.current_file_object()),
            ),
        ]))
    }

    fn push_diagnostic(&mut self, message: String) {
        if !self.diagnostics.iter().any(|existing| existing == &message) {
            self.diagnostics.push(message);
        }
    }
}

#[cfg(feature = "js_runtime")]
impl TemplateSession<'_> {
    fn sync_from_js_runtime(&mut self) {
        if let Some((target_path, target_contents, changed_paths, diagnostics)) =
            self.js_runtime.as_ref().and_then(JsTemplateRuntime::snapshot)
        {
            self.target_path = target_path;
            self.target_contents = target_contents;
            self.changed_paths = changed_paths;
            self.diagnostics = diagnostics;
        }
    }

    fn js_runtime(&mut self) -> Result<&mut JsTemplateRuntime, CliError> {
        if self.js_runtime.is_none() {
            self.js_runtime = Some(JsTemplateRuntime::new(self)?);
        }
        self.js_runtime
            .as_mut()
            .ok_or_else(|| CliError::operation("failed to initialize templater runtime"))
    }

    fn evaluate_js_expression(&mut self, source: &str) -> Result<String, CliError> {
        let result = {
            let runtime = self.js_runtime()?;
            runtime.evaluate_expression(source)?
        };
        self.sync_from_js_runtime();
        Ok(result)
    }

    fn evaluate_js_code(&mut self, source: &str) -> Result<String, CliError> {
        let result = {
            let runtime = self.js_runtime()?;
            runtime.evaluate_code(source)?
        };
        self.sync_from_js_runtime();
        Ok(result)
    }

    fn run_post_render_hooks(&mut self, rendered: String) -> Result<String, CliError> {
        let Some(runtime) = self.js_runtime.as_mut() else {
            return Ok(rendered);
        };
        let rendered = runtime.run_post_render_hooks(&rendered)?;
        self.sync_from_js_runtime();
        Ok(rendered)
    }
}

#[cfg(feature = "js_runtime")]
struct JsTemplateRuntime {
    runtime: rquickjs::Runtime,
    context: rquickjs::Context,
    state: std::sync::Arc<std::sync::Mutex<JsTemplateState>>,
}

#[cfg(feature = "js_runtime")]
impl JsTemplateRuntime {
    fn new(session: &mut TemplateSession<'_>) -> Result<Self, CliError> {
        use rquickjs::function::Func;
        use rquickjs::{Context, Ctx, Runtime};
        use std::sync::{Arc, Mutex};

        let runtime = Runtime::new().map_err(CliError::operation)?;
        let context = Context::full(&runtime).map_err(CliError::operation)?;

        let state = Arc::new(Mutex::new(JsTemplateState::from_session(session)));
        context.with(|ctx| -> Result<(), CliError> {
            let globals = ctx.globals();
            globals
                .set(
                    "__tp_call_json",
                    Func::from({
                        let state = Arc::clone(&state);
                        move |ctx: Ctx<'_>, name: String, args_json: String| -> rquickjs::Result<String> {
                            dispatch_js_call(&ctx, Arc::clone(&state), &name, &args_json)
                        }
                    }),
                )
                .map_err(CliError::operation)?;
            ctx.eval::<(), _>(TEMPLATER_JS_PRELUDE)
                .map_err(CliError::operation)?;
            load_user_scripts(ctx.clone(), Arc::clone(&state))?;
            Ok(())
        })?;

        Ok(Self {
            runtime,
            context,
            state,
        })
    }

    fn evaluate_expression(&mut self, source: &str) -> Result<String, CliError> {
        let source = strip_top_level_await(source);
        let wrapped = format!("__vulcanTemplaterStringify(({source}))");
        let result = self
            .context
            .with(|ctx| ctx.eval::<String, _>(wrapped))
            .map_err(CliError::operation)?;
        drain_jobs(&self.runtime)?;
        Ok(result)
    }

    fn evaluate_code(&mut self, source: &str) -> Result<String, CliError> {
        let source = strip_top_level_await(source);
        self.context
            .with(|ctx| {
                ctx.eval::<(), _>("globalThis.tR = ''")
                    .map_err(CliError::operation)?;
                ctx.eval::<(), _>(source).map_err(CliError::operation)?;
                ctx.eval::<String, _>("String(globalThis.tR ?? '')")
                    .map_err(CliError::operation)
            })
            .and_then(|result| {
                drain_jobs(&self.runtime)?;
                Ok(result)
            })
    }

    fn snapshot(&self) -> Option<(String, String, BTreeSet<String>, Vec<String>)> {
        self.state.lock().ok().map(|state| {
            (
                state.target_path.clone(),
                state.target_contents.clone(),
                state.changed_paths.clone(),
                state.diagnostics.clone(),
            )
        })
    }

    fn run_post_render_hooks(&mut self, rendered: &str) -> Result<String, CliError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CliError::operation("templater lock poisoned"))?;
            state.target_contents = rendered.to_string();
        }
        self.context
            .with(|ctx| {
                ctx.eval::<(), _>(
                    "(async () => { const hooks = globalThis.__tpHooks ?? []; globalThis.__tpHooks = []; for (const hook of hooks) { if (typeof hook === 'function') { await hook(); } } })()",
                )
                .map_err(CliError::operation)
            })?;
        drain_jobs(&self.runtime)?;
        self.state
            .lock()
            .map(|state| state.target_contents.clone())
            .map_err(|_| CliError::operation("templater lock poisoned"))
    }
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, Clone)]
struct JsTemplateState {
    paths: VaultPaths,
    vault_config: VaultConfig,
    templates: Vec<TemplateCandidate>,
    template_path: Option<PathBuf>,
    target_path: String,
    target_contents: String,
    vars: HashMap<String, String>,
    allow_mutations: bool,
    run_mode_code: i64,
    changed_paths: BTreeSet<String>,
    diagnostics: Vec<String>,
}

#[cfg(feature = "js_runtime")]
impl JsTemplateState {
    fn from_session(session: &TemplateSession<'_>) -> Self {
        Self {
            paths: session.request.paths.clone(),
            vault_config: session.request.vault_config.clone(),
            templates: session.request.templates.to_vec(),
            template_path: session.request.template_path.map(Path::to_path_buf),
            target_path: session.target_path.clone(),
            target_contents: session.target_contents.clone(),
            vars: session.request.vars.clone(),
            allow_mutations: session.request.allow_mutations,
            run_mode_code: session.request.run_mode.config_code(),
            changed_paths: session.changed_paths.clone(),
            diagnostics: session.diagnostics.clone(),
        }
    }

    fn push_diagnostic(&mut self, message: String) {
        if !self.diagnostics.iter().any(|existing| existing == &message) {
            self.diagnostics.push(message);
        }
    }
}

#[cfg(feature = "js_runtime")]
const TEMPLATER_JS_PRELUDE: &str = r#"
function __tpCall(name, ...args) {
  const response = JSON.parse(__tp_call_json(name, JSON.stringify(args)));
  if (response.error) {
    throw new Error(response.error);
  }
  return response.value;
}
function __vulcanTemplaterStringify(value) {
  if (value === undefined || value === null) return "";
  if (Array.isArray(value)) return value.map(__vulcanTemplaterStringify).join(",");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}
const tp = {
  date: {
    now: (...args) => __tpCall("date.now", ...args),
    tomorrow: (...args) => __tpCall("date.tomorrow", ...args),
    yesterday: (...args) => __tpCall("date.yesterday", ...args),
    weekday: (...args) => __tpCall("date.weekday", ...args),
  },
  file: {
    get title() { return __tpCall("file.title"); },
    path: (absolute = false) => __tpCall("file.path", Boolean(absolute)),
    folder: (absolute = false) => __tpCall("file.folder", Boolean(absolute)),
    creation_date: (format) => __tpCall("file.creation_date", format ?? null),
    last_modified_date: (format) => __tpCall("file.last_modified_date", format ?? null),
    get content() { return __tpCall("file.content"); },
    get tags() { return __tpCall("file.tags"); },
    exists: (path) => __tpCall("file.exists", String(path)),
    include: (path) => __tpCall("file.include", String(path)),
    create_new: (template, filename, openNew = false, folder = "") => __tpCall("file.create_new", template, filename ?? "", Boolean(openNew), folder ?? ""),
    move: (path) => __tpCall("file.move", String(path)),
    rename: (name) => __tpCall("file.rename", String(name)),
    cursor: (order = null) => __tpCall("file.cursor", order),
    find_tfile: (path) => __tpCall("file.find_tfile", String(path)),
  },
  get frontmatter() { return __tpCall("frontmatter"); },
  system: {
    prompt: (...args) => __tpCall("system.prompt", ...args),
    suggester: (...args) => __tpCall("system.suggester", ...args),
    clipboard: () => __tpCall("system.clipboard"),
  },
  web: {
    request: (...args) => __tpCall("web.request", ...args),
    daily_quote: () => __tpCall("web.daily_quote"),
    random_picture: (...args) => __tpCall("web.random_picture", ...args),
  },
  user: {},
  hooks: {
    on_all_templates_executed: (callback) => {
      globalThis.__tpHooks = globalThis.__tpHooks ?? [];
      globalThis.__tpHooks.push(callback);
    },
  },
  get config() { return __tpCall("config"); },
  obsidian: {
    normalizePath: (path) => __tpCall("obsidian.normalizePath", String(path)),
    htmlToMarkdown: (html) => __tpCall("obsidian.htmlToMarkdown", String(html)),
    requestUrl: (url) => __tpCall("obsidian.requestUrl", String(url)),
  },
};
tp.app = new Proxy({}, {
  get() {
    __tpCall("diagnostic", "tp.app.* is not available in CLI templates");
    return undefined;
  }
});
globalThis.tp = tp;
"#;

#[cfg(feature = "js_runtime")]
fn load_user_scripts(
    ctx: rquickjs::Ctx<'_>,
    state: std::sync::Arc<std::sync::Mutex<JsTemplateState>>,
) -> Result<(), CliError> {
    let (user_scripts_folder, templates_pairs, enable_system_commands) = {
        let state = state
            .lock()
            .map_err(|_| CliError::operation("templater lock poisoned"))?;
        (
            state.vault_config.templates.user_scripts_folder.clone(),
            state.vault_config.templates.templates_pairs.clone(),
            state.vault_config.templates.enable_system_commands,
        )
    };
    let globals = ctx.globals();
    let tp: rquickjs::Object<'_> = globals.get("tp").map_err(CliError::operation)?;
    let user: rquickjs::Object<'_> = tp.get("user").map_err(CliError::operation)?;

    if let Some(folder) = user_scripts_folder {
        let absolute = {
            let state = state
                .lock()
                .map_err(|_| CliError::operation("templater lock poisoned"))?;
            state.paths.vault_root().join(folder)
        };
        if absolute.is_dir() {
            for entry in fs::read_dir(&absolute).map_err(CliError::operation)? {
                let entry = entry.map_err(CliError::operation)?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("js") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                let script = fs::read_to_string(&path).map_err(CliError::operation)?;
                let wrapper = format!(
                    "(function(){{ const module = {{ exports: {{}} }}; const exports = module.exports; {script}; return module.exports; }})()"
                );
                let exported: rquickjs::Value<'_> = ctx.eval(wrapper).map_err(CliError::operation)?;
                user.set(stem, exported).map_err(CliError::operation)?;
            }
        }
    }

    if enable_system_commands {
        for pair in templates_pairs {
            let function = ctx
                .eval::<rquickjs::Function<'_>, _>(format!(
                    "(function(...args) {{ return __tpCall({:?}, ...args); }})",
                    pair.name
                ))
                .map_err(CliError::operation)?;
            user.set(pair.name, function).map_err(CliError::operation)?;
        }
    }
    Ok(())
}

#[cfg(feature = "js_runtime")]
fn drain_jobs(runtime: &rquickjs::Runtime) -> Result<(), CliError> {
    while runtime.is_job_pending() {
        runtime
            .execute_pending_job()
            .map_err(|_| CliError::operation("failed to execute pending templater JS job"))?;
    }
    Ok(())
}

#[cfg(feature = "js_runtime")]
fn dispatch_js_call(
    ctx: &rquickjs::Ctx<'_>,
    state: std::sync::Arc<std::sync::Mutex<JsTemplateState>>,
    name: &str,
    args_json: &str,
) -> rquickjs::Result<String> {
    let args: Vec<JsonValue> =
        serde_json::from_str(args_json).map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))?;
    let response = js_call_response(state, name, &args)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error))?;
    serde_json::to_string(&response)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))
}

#[cfg(feature = "js_runtime")]
fn js_call_response(
    state: std::sync::Arc<std::sync::Mutex<JsTemplateState>>,
    name: &str,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    let mut state = state.lock().map_err(|_| "templater lock poisoned".to_string())?;
    let value = match name {
        "diagnostic" => {
            if let Some(message) = args.first().and_then(JsonValue::as_str) {
                state.push_diagnostic(message.to_string());
            }
            JsonValue::Null
        }
        "date.now" => JsonValue::String(js_date_now(&state, args)?),
        "date.tomorrow" => JsonValue::String(js_date_fixed_offset(args, 1)),
        "date.yesterday" => JsonValue::String(js_date_fixed_offset(args, -1)),
        "date.weekday" => JsonValue::String(js_date_weekday(&state, args)?),
        "file.title" => JsonValue::String(
            Path::new(&state.target_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_string(),
        ),
        "file.path" => JsonValue::String(js_file_path(&state, args)),
        "file.folder" => JsonValue::String(js_file_folder(&state, args)),
        "file.creation_date" => JsonValue::String(js_file_timestamp(&state, args, true)),
        "file.last_modified_date" => JsonValue::String(js_file_timestamp(&state, args, false)),
        "file.content" => JsonValue::String(state.target_contents.clone()),
        "file.tags" => JsonValue::Array(
            parse_document(&state.target_contents, &state.vault_config)
                .tags
                .iter()
                .map(|tag| JsonValue::String(tag.tag_text.clone()))
                .collect(),
        ),
        "file.exists" => JsonValue::Bool(
            args.first()
                .and_then(JsonValue::as_str)
                .and_then(|path| resolve_vault_path_from_state(&state, path))
                .is_some(),
        ),
        "file.include" => JsonValue::String(js_file_include(&state, args)?),
        "file.create_new" => js_file_create_new(&mut state, args)?,
        "file.move" => {
            js_file_move(&mut state, args)?;
            JsonValue::String(String::new())
        }
        "file.rename" => {
            js_file_rename(&mut state, args)?;
            JsonValue::String(String::new())
        }
        "file.cursor" => JsonValue::String(String::new()),
        "file.find_tfile" => args
            .first()
            .and_then(JsonValue::as_str)
            .and_then(|path| resolve_vault_path_from_state(&state, path))
            .as_deref()
            .map(file_object_json)
            .map(JsonValue::Object)
            .unwrap_or(JsonValue::Null),
        "frontmatter" => JsonValue::Object(yaml_mapping_to_json_object(
            &current_frontmatter_mapping(&state.target_contents).unwrap_or_default(),
        )),
        "config" => JsonValue::Object(JsonMap::from_iter([
            (
                "template_file".to_string(),
                state
                    .template_path
                    .as_deref()
                    .map(path_to_relative_file_json)
                    .map(JsonValue::Object)
                    .unwrap_or(JsonValue::Null),
            ),
            (
                "target_file".to_string(),
                JsonValue::Object(file_object_json(&state.target_path)),
            ),
            (
                "run_mode".to_string(),
                JsonValue::Number(state.run_mode_code.into()),
            ),
            (
                "active_file".to_string(),
                JsonValue::Object(file_object_json(&state.target_path)),
            ),
        ])),
        "system.prompt" => JsonValue::String(js_system_prompt(&state, args)),
        "system.suggester" => js_system_suggester(&state, args),
        "system.clipboard" => JsonValue::String(read_clipboard_best_effort()),
        "web.request" | "obsidian.requestUrl" => {
            let url = args.first().and_then(JsonValue::as_str).unwrap_or_default();
            let path = args.get(1).and_then(JsonValue::as_str);
            JsonValue::String(js_web_request_string(&state, url, path)?)
        }
        "web.daily_quote" => JsonValue::String(js_daily_quote(&state)?),
        "web.random_picture" => JsonValue::String(random_picture_markdown(
            args.first().and_then(JsonValue::as_str),
            args.get(1).and_then(JsonValue::as_str),
            args.get(2).and_then(JsonValue::as_bool).unwrap_or(false),
        )),
        "obsidian.normalizePath" => JsonValue::String(normalize_note_output_path(
            args.first().and_then(JsonValue::as_str).unwrap_or_default(),
        )?),
        "obsidian.htmlToMarkdown" => JsonValue::String(html_to_markdown(
            args.first().and_then(JsonValue::as_str).unwrap_or_default(),
        )),
        other => js_system_user_command(&state, other, args)?,
    };
    Ok(JsonValue::Object(JsonMap::from_iter([
        ("value".to_string(), value),
        ("error".to_string(), JsonValue::Null),
    ])))
}

#[cfg(feature = "js_runtime")]
fn js_reference_timestamp(
    _state: &JsTemplateState,
    reference: Option<&str>,
    reference_format: Option<&str>,
) -> Result<i64, String> {
    if let Some(reference) = reference {
        if let Some(format) = reference_format {
            return parse_date_with_format(reference, format)
                .ok_or_else(|| format!("failed to parse reference date `{reference}`"));
        }
        return parse_date_like_string(reference)
            .ok_or_else(|| format!("failed to parse reference date `{reference}`"));
    }
    Ok(current_timestamp_millis())
}

#[cfg(feature = "js_runtime")]
fn js_apply_offset(
    millis: i64,
    offset: Option<&JsonValue>,
) -> Result<i64, String> {
    match offset {
        None => Ok(millis),
        Some(JsonValue::Null) => Ok(millis),
        Some(JsonValue::Number(value)) => {
            let days = value.as_f64().unwrap_or_default();
            #[allow(clippy::cast_possible_truncation)]
            {
                Ok(millis + (days * 86_400_000.0) as i64)
            }
        }
        Some(JsonValue::String(text)) => apply_offset_string(millis, text),
        Some(_) => Err("invalid templater date offset".to_string()),
    }
}

#[cfg(feature = "js_runtime")]
fn js_web_request_string(
    state: &JsTemplateState,
    url: &str,
    json_path: Option<&str>,
) -> Result<String, String> {
    ensure_allowlisted_url(&state.vault_config.templates, url)?;
    let response = reqwest::blocking::get(url)
        .map_err(|error| error.to_string())?
        .text()
        .map_err(|error| error.to_string())?;
    extract_web_response(response, json_path)
}

#[cfg(feature = "js_runtime")]
fn js_date_now(state: &JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    let millis = js_reference_timestamp(
        state,
        args.get(2).and_then(JsonValue::as_str),
        args.get(3).and_then(JsonValue::as_str),
    )?;
    let millis = js_apply_offset(millis, args.get(1))?;
    Ok(format_date(millis, format))
}

#[cfg(feature = "js_runtime")]
fn js_date_fixed_offset(args: &[JsonValue], day_offset: i64) -> String {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    format_date(current_timestamp_millis() + day_offset * 86_400_000, format)
}

#[cfg(feature = "js_runtime")]
fn js_date_weekday(state: &JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    let weekday = args.get(1).and_then(JsonValue::as_i64).unwrap_or_default();
    let millis = js_reference_timestamp(
        state,
        args.get(2).and_then(JsonValue::as_str),
        args.get(3).and_then(JsonValue::as_str),
    )?;
    Ok(format_date(weekday_timestamp(millis, weekday), format))
}

#[cfg(feature = "js_runtime")]
fn js_file_path(state: &JsTemplateState, args: &[JsonValue]) -> String {
    if args.first().and_then(JsonValue::as_bool).unwrap_or(false) {
        state
            .paths
            .vault_root()
            .join(&state.target_path)
            .display()
            .to_string()
    } else {
        state.target_path.clone()
    }
}

#[cfg(feature = "js_runtime")]
fn js_file_folder(state: &JsTemplateState, args: &[JsonValue]) -> String {
    let relative = Path::new(&state.target_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if args.first().and_then(JsonValue::as_bool).unwrap_or(false) {
        state.paths.vault_root().join(relative).display().to_string()
    } else {
        relative
    }
}

#[cfg(feature = "js_runtime")]
fn js_file_timestamp(state: &JsTemplateState, args: &[JsonValue], creation: bool) -> String {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_FILE_DATE_FORMAT);
    let millis = file_timestamp_millis(&state.paths.vault_root().join(&state.target_path), creation)
        .unwrap_or_else(current_timestamp_millis);
    format_date(millis, format)
}

#[cfg(feature = "js_runtime")]
fn js_file_include(state: &JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    let path = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    let target = resolve_vault_path_from_state(state, path)
        .ok_or_else(|| format!("File {path} doesn't exist"))?;
    fs::read_to_string(state.paths.vault_root().join(target)).map_err(|error| error.to_string())
}

#[cfg(feature = "js_runtime")]
fn js_file_create_new(
    state: &mut JsTemplateState,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    if !state.allow_mutations {
        state.push_diagnostic("tp.file.create_new() is disabled during template preview".to_string());
        return Ok(JsonValue::Null);
    }
    let content = args
        .first()
        .map(json_value_to_js_string)
        .unwrap_or_default();
    let filename = args.get(1).and_then(JsonValue::as_str).unwrap_or("Untitled");
    let folder = args.get(3).and_then(JsonValue::as_str).unwrap_or_default();
    let path = if folder.trim().is_empty() {
        format!("{filename}.md")
    } else {
        format!("{}/{}.md", folder.trim_matches('/'), filename)
    };
    let normalized = normalize_note_output_path(&path)?;
    let absolute = state.paths.vault_root().join(&normalized);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&absolute, content).map_err(|error| error.to_string())?;
    state.changed_paths.insert(normalized.clone());
    Ok(JsonValue::Object(file_object_json(&normalized)))
}

#[cfg(feature = "js_runtime")]
fn js_file_move(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<(), String> {
    if !state.allow_mutations {
        state.push_diagnostic("tp.file.move() is disabled during template preview".to_string());
        return Ok(());
    }
    let new_path = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    let normalized = normalize_note_output_path(new_path)?;
    if state.paths.vault_root().join(&state.target_path).is_file() {
        let summary = move_note(&state.paths, &state.target_path, &normalized, false)
            .map_err(|error| error.to_string())?;
        state.changed_paths.insert(summary.destination_path.clone());
        for rewritten in summary.rewritten_files {
            state.changed_paths.insert(rewritten.path);
        }
    }
    state.target_path = normalized;
    Ok(())
}

#[cfg(feature = "js_runtime")]
fn js_file_rename(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<(), String> {
    let name = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    if name.contains(['\\', '/', ':']) {
        return Err("File name cannot contain any of these characters: \\ / :".to_string());
    }
    let folder = Path::new(&state.target_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let target = if folder.is_empty() {
        format!("{name}.md")
    } else {
        format!("{folder}/{name}.md")
    };
    js_file_move(state, &[JsonValue::String(target)])
}

#[cfg(feature = "js_runtime")]
fn js_system_prompt(state: &JsTemplateState, args: &[JsonValue]) -> String {
    let prompt = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    let default = args.get(1).and_then(JsonValue::as_str);
    let slug = slugify_var_key(prompt);
    state
        .vars
        .get(prompt)
        .cloned()
        .or_else(|| state.vars.get(&slug).cloned())
        .or_else(|| default.map(ToOwned::to_owned))
        .unwrap_or_default()
}

#[cfg(feature = "js_runtime")]
fn js_system_suggester(state: &JsTemplateState, args: &[JsonValue]) -> JsonValue {
    if let Some(selected) = state.vars.get("suggester") {
        return JsonValue::String(selected.clone());
    }
    args.get(1)
        .and_then(JsonValue::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(JsonValue::Null)
}

#[cfg(feature = "js_runtime")]
fn js_system_user_command(
    state: &JsTemplateState,
    name: &str,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    let Some(pair) = state
        .vault_config
        .templates
        .templates_pairs
        .iter()
        .find(|pair| pair.name == name)
    else {
        return Err(format!("unsupported templater JS call: {name}"));
    };
    if !state.vault_config.templates.enable_system_commands {
        return Err(format!(
            "templater system command user function `{name}` is disabled in config"
        ));
    }
    let rendered_command = render_template_request(TemplateRenderRequest {
        paths: &state.paths,
        vault_config: &state.vault_config,
        templates: &state.templates,
        template_path: None,
        template_text: &pair.command,
        target_path: &state.target_path,
        target_contents: Some(&state.target_contents),
        engine: TemplateEngineKind::Templater,
        vars: &state.vars,
        allow_mutations: state.allow_mutations,
        run_mode: template_run_mode_from_code(state.run_mode_code),
    })
    .map_err(|error| error.to_string())?;
    let shell = state.vault_config.templates.shell_path.clone();
    let env_json = args.first().cloned().unwrap_or(JsonValue::Null);
    let mut process = ProcessCommand::new(shell.as_deref().unwrap_or(default_system_shell()));
    configure_shell_command(&mut process, shell.as_deref(), &rendered_command.content);
    process.current_dir(state.paths.vault_root());
    if let Some(env) = env_json.as_object() {
        for (key, value) in env {
            process.env(key, json_value_to_js_string(value));
        }
    }
    let output = process.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("templater system command `{name}` failed"));
    }
    Ok(JsonValue::String(
        String::from_utf8_lossy(&output.stdout).trim_end().to_string(),
    ))
}

#[cfg(feature = "js_runtime")]
fn json_value_to_js_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(items) => items
            .iter()
            .map(json_value_to_js_string)
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(object) => JsonValue::Object(object.clone()).to_string(),
    }
}

#[cfg(feature = "js_runtime")]
fn extract_web_response(body: String, json_path: Option<&str>) -> Result<String, String> {
    let Some(path) = json_path.filter(|value| !value.trim().is_empty()) else {
        return Ok(body);
    };
    let mut value: JsonValue = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    for segment in path.split('.') {
        value = match value {
            JsonValue::Array(items) => {
                let index = segment.parse::<usize>().map_err(|_| {
                    format!("json_path segment `{segment}` is not a valid array index")
                })?;
                items.get(index).cloned().unwrap_or(JsonValue::Null)
            }
            JsonValue::Object(object) => object.get(segment).cloned().unwrap_or(JsonValue::Null),
            _ => JsonValue::Null,
        };
    }
    Ok(match value {
        JsonValue::String(text) => text,
        other => other.to_string(),
    })
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, serde::Deserialize)]
struct DailyQuote {
    quote: String,
    author: String,
}

#[derive(Debug, Clone, PartialEq)]
enum TemplateValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<TemplateValue>),
    Object(JsonMap<String, JsonValue>),
}

impl TemplateValue {
    fn from_json(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(value),
            JsonValue::Number(value) => Self::Number(value.as_f64().unwrap_or_default()),
            JsonValue::String(value) => Self::String(value),
            JsonValue::Array(items) => {
                Self::Array(items.into_iter().map(Self::from_json).collect())
            }
            JsonValue::Object(object) => Self::Object(object),
        }
    }

    fn as_string_list(&self) -> Vec<String> {
        match self {
            Self::Array(items) => items.iter().map(template_value_to_string).collect(),
            Self::String(value) => vec![value.clone()],
            _ => Vec::new(),
        }
    }
}

fn template_value_to_string(value: &TemplateValue) -> String {
    match value {
        TemplateValue::Null => String::new(),
        TemplateValue::Bool(value) => value.to_string(),
        TemplateValue::Number(value) => {
            if value.fract() == 0.0 {
                format!("{value:.0}")
            } else {
                value.to_string()
            }
        }
        TemplateValue::String(value) => value.clone(),
        TemplateValue::Array(values) => values
            .iter()
            .map(template_value_to_string)
            .collect::<Vec<_>>()
            .join(","),
        TemplateValue::Object(value) => JsonValue::Object(value.clone()).to_string(),
    }
}

#[derive(Debug)]
enum NativeExpressionError {
    Message(String),
    RequiresJsRuntime(String),
}

#[derive(Debug, Clone, PartialEq)]
enum NativeExpression {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
    Path(Vec<NativePathPart>),
    Call {
        callee: Vec<NativePathPart>,
        args: Vec<NativeExpression>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NativePathPart {
    Name(String),
    Index(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrimMode {
    None,
    Newline,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplaterTag<'a> {
    body: &'a str,
    execution: bool,
    dynamic: bool,
    left_trim: TrimMode,
    right_trim: TrimMode,
}

fn parse_templater_tag(source: &str, start: usize) -> Option<(TemplaterTag<'_>, usize)> {
    let rest = source.get(start + 2..)?;
    let mut left_trim = TrimMode::None;
    let mut body_start = 0_usize;
    if let Some(marker) = rest.as_bytes().first().copied() {
        left_trim = trim_mode_from_marker(marker);
        if left_trim != TrimMode::None {
            body_start += 1;
        }
    }

    let mut execution = false;
    let mut dynamic = false;
    if let Some(marker) = rest[body_start..].as_bytes().first().copied() {
        match marker {
            b'*' => {
                execution = true;
                body_start += 1;
            }
            b'+' => {
                dynamic = true;
                body_start += 1;
            }
            _ => {}
        }
    }

    let close = rest[body_start..].find("%>")?;
    let close_index = body_start + close;
    let body = &rest[body_start..close_index];
    let (body, right_trim) = if let Some(last) = body.as_bytes().last().copied() {
        let trim = trim_mode_from_marker(last);
        if trim == TrimMode::None {
            (body.trim(), TrimMode::None)
        } else {
            (body[..body.len().saturating_sub(1)].trim(), trim)
        }
    } else {
        ("", TrimMode::None)
    };
    Some((
        TemplaterTag {
            body,
            execution,
            dynamic,
            left_trim,
            right_trim,
        },
        start + 2 + close_index + 2,
    ))
}

fn trim_mode_from_marker(marker: u8) -> TrimMode {
    match marker {
        b'-' => TrimMode::Newline,
        b'_' => TrimMode::All,
        _ => TrimMode::None,
    }
}

fn apply_left_trim(output: &mut String, trim: TrimMode) {
    match trim {
        TrimMode::None => {}
        TrimMode::All => {
            *output = output.trim_end().to_string();
        }
        TrimMode::Newline => trim_single_trailing_newline(output),
    }
}

fn apply_right_trim(source: &str, mut cursor: usize, trim: TrimMode) -> usize {
    match trim {
        TrimMode::None => {}
        TrimMode::All => {
            while let Some(character) = source[cursor..].chars().next() {
                if character.is_whitespace() {
                    cursor += character.len_utf8();
                } else {
                    break;
                }
            }
        }
        TrimMode::Newline => {
            if source[cursor..].starts_with("\r\n") {
                cursor += 2;
            } else if source[cursor..].starts_with('\n') {
                cursor += 1;
            }
        }
    }
    cursor
}

fn trim_single_trailing_newline(output: &mut String) {
    if output.ends_with("\r\n") {
        output.truncate(output.len().saturating_sub(2));
    } else if output.ends_with('\n') {
        output.truncate(output.len().saturating_sub(1));
    }
}

fn parse_native_expression(source: &str) -> Result<NativeExpression, NativeExpressionError> {
    let mut parser = NativeExpressionParser::new(source);
    let expression = parser.parse_expression()?;
    parser.skip_ws();
    if !parser.is_done() {
        return Err(NativeExpressionError::RequiresJsRuntime(
            "templater expression requires the `js_runtime` feature".to_string(),
        ));
    }
    Ok(expression)
}

struct NativeExpressionParser<'a> {
    source: &'a str,
    cursor: usize,
}

impl<'a> NativeExpressionParser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, cursor: 0 }
    }

    fn is_done(&self) -> bool {
        self.cursor >= self.source.len()
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.cursor += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.cursor..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.cursor += ch.len_utf8();
        Some(ch)
    }

    fn parse_expression(&mut self) -> Result<NativeExpression, NativeExpressionError> {
        self.skip_ws();
        let Some(ch) = self.peek_char() else {
            return Ok(NativeExpression::Null);
        };
        match ch {
            '"' | '\'' => self.parse_string().map(NativeExpression::String),
            '-' | '0'..='9' => self.parse_number().map(NativeExpression::Number),
            _ => self.parse_identifier_path_or_call(),
        }
    }

    fn parse_string(&mut self) -> Result<String, NativeExpressionError> {
        let Some(quote) = self.consume_char() else {
            return Ok(String::new());
        };
        let mut result = String::new();
        while let Some(ch) = self.consume_char() {
            if ch == quote {
                return Ok(result);
            }
            if ch == '\\' {
                if let Some(escaped) = self.consume_char() {
                    result.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                }
            } else {
                result.push(ch);
            }
        }
        Err(NativeExpressionError::Message(
            "unterminated string literal".to_string(),
        ))
    }

    fn parse_number(&mut self) -> Result<f64, NativeExpressionError> {
        let start = self.cursor;
        if self.peek_char() == Some('-') {
            self.consume_char();
        }
        while matches!(self.peek_char(), Some('0'..='9' | '.')) {
            self.consume_char();
        }
        self.source[start..self.cursor]
            .parse::<f64>()
            .map_err(|error| NativeExpressionError::Message(error.to_string()))
    }

    fn parse_identifier_path_or_call(&mut self) -> Result<NativeExpression, NativeExpressionError> {
        let path = self.parse_path()?;
        self.skip_ws();
        if self.peek_char() == Some('(') {
            self.consume_char();
            let mut args = Vec::new();
            loop {
                self.skip_ws();
                if self.peek_char() == Some(')') {
                    self.consume_char();
                    break;
                }
                args.push(self.parse_expression()?);
                self.skip_ws();
                match self.peek_char() {
                    Some(',') => {
                        self.consume_char();
                    }
                    Some(')') => {
                        self.consume_char();
                        break;
                    }
                    _ => {
                        return Err(NativeExpressionError::RequiresJsRuntime(
                            "templater expression requires the `js_runtime` feature".to_string(),
                        ));
                    }
                }
            }
            Ok(NativeExpression::Call { callee: path, args })
        } else {
            if matches!(path_name(&path, 0), Some("true")) && path.len() == 1 {
                Ok(NativeExpression::Bool(true))
            } else if matches!(path_name(&path, 0), Some("false")) && path.len() == 1 {
                Ok(NativeExpression::Bool(false))
            } else if matches!(path_name(&path, 0), Some("null")) && path.len() == 1 {
                Ok(NativeExpression::Null)
            } else {
                Ok(NativeExpression::Path(path))
            }
        }
    }

    fn parse_path(&mut self) -> Result<Vec<NativePathPart>, NativeExpressionError> {
        let mut path = vec![NativePathPart::Name(self.parse_identifier()?)];
        loop {
            self.skip_ws();
            match self.peek_char() {
                Some('.') => {
                    self.consume_char();
                    path.push(NativePathPart::Name(self.parse_identifier()?));
                }
                Some('[') => {
                    self.consume_char();
                    self.skip_ws();
                    let key = self.parse_string()?;
                    self.skip_ws();
                    if self.consume_char() != Some(']') {
                        return Err(NativeExpressionError::Message(
                            "unterminated bracket access".to_string(),
                        ));
                    }
                    path.push(NativePathPart::Index(key));
                }
                _ => break,
            }
        }
        Ok(path)
    }

    fn parse_identifier(&mut self) -> Result<String, NativeExpressionError> {
        self.skip_ws();
        let start = self.cursor;
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.consume_char();
            } else {
                break;
            }
        }
        if start == self.cursor {
            return Err(NativeExpressionError::RequiresJsRuntime(
                "templater expression requires the `js_runtime` feature".to_string(),
            ));
        }
        Ok(self.source[start..self.cursor].to_string())
    }
}

fn path_name(path: &[NativePathPart], index: usize) -> Option<&str> {
    match path.get(index) {
        Some(NativePathPart::Name(name) | NativePathPart::Index(name)) => Some(name),
        None => None,
    }
}

fn join_native_path(path: &[NativePathPart]) -> String {
    let mut rendered = String::new();
    for (index, part) in path.iter().enumerate() {
        match part {
            NativePathPart::Name(name) => {
                if index > 0 {
                    rendered.push('.');
                }
                rendered.push_str(name);
            }
            NativePathPart::Index(name) => {
                rendered.push_str("[\"");
                rendered.push_str(name);
                rendered.push_str("\"]");
            }
        }
    }
    rendered
}

fn string_arg(args: &[NativeExpression], index: usize) -> Option<String> {
    match args.get(index) {
        Some(NativeExpression::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn number_arg(args: &[NativeExpression], index: usize) -> Option<i64> {
    match args.get(index) {
        Some(NativeExpression::Number(value)) => Some(*value as i64),
        _ => None,
    }
}

fn bool_arg(args: &[NativeExpression], index: usize) -> Option<bool> {
    match args.get(index) {
        Some(NativeExpression::Bool(value)) => Some(*value),
        _ => None,
    }
}

#[cfg(feature = "js_runtime")]
fn template_run_mode_from_code(code: i64) -> TemplateRunMode {
    match code {
        1 => TemplateRunMode::Append,
        5 => TemplateRunMode::Dynamic,
        _ => TemplateRunMode::Create,
    }
}

fn normalize_note_output_path(path: &str) -> Result<String, String> {
    vulcan_core::paths::normalize_relative_input_path(
        path,
        vulcan_core::paths::RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| error.to_string())
}

fn yaml_mapping_to_json_object(mapping: &YamlMapping) -> JsonMap<String, JsonValue> {
    JsonMap::from_iter(mapping.iter().filter_map(|(key, value)| {
        key.as_str()
            .map(|key| (key.to_string(), yaml_to_json(value)))
    }))
}

fn yaml_to_json(value: &YamlValue) -> JsonValue {
    match value {
        YamlValue::Null => JsonValue::Null,
        YamlValue::Bool(value) => JsonValue::Bool(*value),
        YamlValue::Number(value) => value
            .as_i64()
            .map(Into::into)
            .map(JsonValue::Number)
            .or_else(|| value.as_f64().and_then(serde_json::Number::from_f64).map(JsonValue::Number))
            .unwrap_or(JsonValue::Null),
        YamlValue::String(value) => JsonValue::String(value.clone()),
        YamlValue::Sequence(items) => {
            JsonValue::Array(items.iter().map(yaml_to_json).collect())
        }
        YamlValue::Mapping(mapping) => JsonValue::Object(yaml_mapping_to_json_object(mapping)),
        _ => JsonValue::Null,
    }
}

fn current_frontmatter_mapping(source: &str) -> Option<YamlMapping> {
    parse_frontmatter_document(source, false)
        .ok()
        .and_then(|(frontmatter, _)| frontmatter)
}

fn merge_yaml_mappings(target: &mut YamlMapping, source: &YamlMapping) {
    for (key, value) in source {
        if !target.contains_key(key) {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn prompt_lookup_key(prompt: &str, ordinal: usize) -> PromptLookupKey {
    PromptLookupKey {
        raw: prompt.to_string(),
        slug: if prompt.trim().is_empty() {
            format!("prompt_{ordinal}")
        } else {
            slugify_var_key(prompt)
        },
    }
}

struct PromptLookupKey {
    raw: String,
    slug: String,
}

fn lookup_template_var(
    vars: &HashMap<String, String>,
    key: &PromptLookupKey,
    exact_fallback: Option<&str>,
) -> Option<String> {
    vars.get(&key.raw)
        .cloned()
        .or_else(|| exact_fallback.and_then(|value| vars.get(value).cloned()))
        .or_else(|| vars.get(&key.slug).cloned())
}

fn slugify_var_key(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('_');
            last_was_separator = true;
        }
    }
    slug.trim_matches('_').to_string()
}

fn read_prompt_value(prompt: &str, default: Option<&str>) -> Result<Option<String>, String> {
    let prompt = if prompt.trim().is_empty() {
        "Value"
    } else {
        prompt.trim()
    };
    let suffix = default.map_or(String::new(), |value| format!(" [{value}]"));
    eprint!("{prompt}{suffix}: ");
    io::stderr().flush().map_err(|error| error.to_string())?;
    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .map_err(|error| error.to_string())?;
    let value = buffer.trim_end_matches(['\n', '\r']).trim().to_string();
    if value.is_empty() {
        Ok(default.map(ToOwned::to_owned))
    } else {
        Ok(Some(value))
    }
}

fn read_suggester_value(prompt: &str, items: &[String]) -> Result<Option<String>, String> {
    if items.is_empty() {
        return Ok(None);
    }
    if !prompt.trim().is_empty() {
        eprintln!("{prompt}");
    }
    for (index, item) in items.iter().enumerate() {
        eprintln!("{}: {}", index + 1, item);
    }
    eprint!("Select item [1]: ");
    io::stderr().flush().map_err(|error| error.to_string())?;
    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .map_err(|error| error.to_string())?;
    let selected = buffer.trim();
    if selected.is_empty() {
        return Ok(items.first().cloned());
    }
    let index = selected.parse::<usize>().map_err(|error| error.to_string())?;
    Ok(items.get(index.saturating_sub(1)).cloned())
}

fn read_clipboard_best_effort() -> String {
    clipboard_commands()
        .into_iter()
        .find_map(|command| run_clipboard_command(&command).ok())
        .unwrap_or_default()
}

fn clipboard_commands() -> Vec<Vec<&'static str>> {
    #[cfg(target_os = "macos")]
    {
        vec![vec!["pbpaste"]]
    }
    #[cfg(target_os = "windows")]
    {
        vec![vec!["powershell", "-NoProfile", "-Command", "Get-Clipboard"]]
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        vec![
            vec!["wl-paste", "-n"],
            vec!["xclip", "-selection", "clipboard", "-o"],
            vec!["xsel", "--clipboard", "--output"],
        ]
    }
}

fn run_clipboard_command(command: &[&str]) -> Result<String, String> {
    let Some(program) = command.first() else {
        return Err("clipboard command missing program".to_string());
    };
    let output = ProcessCommand::new(program)
        .args(&command[1..])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("clipboard command failed: {command:?}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

#[cfg(feature = "js_runtime")]
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

#[cfg(feature = "js_runtime")]
fn configure_shell_command(process: &mut ProcessCommand, shell: Option<&Path>, command: &str) {
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

fn apply_date_offset(
    millis: i64,
    offset: Option<&NativeExpression>,
) -> Result<i64, NativeExpressionError> {
    match offset {
        None => Ok(millis),
        Some(NativeExpression::Number(days)) => {
            #[allow(clippy::cast_possible_truncation)]
            {
                Ok(millis + (days * 86_400_000.0) as i64)
            }
        }
        Some(NativeExpression::String(value)) => {
            apply_offset_string(millis, value).map_err(|error| {
                NativeExpressionError::Message(format!("invalid date offset `{value}`: {error}"))
            })
        }
        Some(_) => Err(NativeExpressionError::Message(
            "date offset must be a number or ISO 8601 duration string".to_string(),
        )),
    }
}

fn apply_offset_string(millis: i64, offset: &str) -> Result<i64, String> {
    let offset = offset.trim();
    if let Some(days) = parse_iso_day_offset(offset) {
        return Ok(millis + days * 86_400_000);
    }
    Err("expected values like P1D, P-1M, or P1Y".to_string())
}

fn parse_iso_day_offset(input: &str) -> Option<i64> {
    let trimmed = input.trim().strip_prefix('P')?;
    let (number, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let value = number.parse::<i64>().ok()?;
    let multiplier = match unit {
        "D" => 1,
        "W" => 7,
        "M" => 30,
        "Y" => 365,
        _ => return None,
    };
    Some(value * multiplier)
}

fn weekday_timestamp(reference: i64, weekday: i64) -> i64 {
    let (year, month, day, _, _, _, _) = vulcan_core::expression::functions::date_components(reference);
    let current = iso_weekday(year, month, day);
    let monday_based = current - 1;
    let delta = weekday - monday_based;
    reference + delta * 86_400_000
}

fn iso_weekday(year: i64, month: i64, day: i64) -> i64 {
    let days = days_from_civil(year, month, day);
    ((days + 3).rem_euclid(7)) + 1
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn file_timestamp_millis(path: &Path, creation: bool) -> Option<i64> {
    let metadata = fs::metadata(path).ok()?;
    let system_time = if creation {
        metadata.created().ok().or_else(|| metadata.modified().ok())
    } else {
        metadata.modified().ok()
    }?;
    let duration = system_time.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

#[cfg(feature = "js_runtime")]
fn current_timestamp_millis() -> i64 {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or_default()
}

#[cfg(feature = "js_runtime")]
fn js_daily_quote(state: &JsTemplateState) -> Result<String, String> {
    const DAILY_QUOTE_URL: &str =
        "https://raw.githubusercontent.com/Zachatoo/quotes-database/refs/heads/main/quotes.json";

    ensure_allowlisted_url(&state.vault_config.templates, DAILY_QUOTE_URL)?;
    let body = reqwest::blocking::get(DAILY_QUOTE_URL)
        .map_err(|error| error.to_string())?
        .text()
        .map_err(|error| error.to_string())?;
    let quotes: Vec<DailyQuote> = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let Some(quote_count) = i64::try_from(quotes.len()).ok().filter(|count| *count > 0) else {
        return Err("templater daily quote source returned no quotes".to_string());
    };
    let day_index = (current_timestamp_millis() / 86_400_000).rem_euclid(quote_count);
    let quote = &quotes[usize::try_from(day_index).unwrap_or_default()];
    Ok(format!(
        "> [!quote] {}\n> — {}",
        quote.quote.trim(),
        quote.author.trim()
    ))
}

fn file_object_json(path: &str) -> JsonMap<String, JsonValue> {
    let relative = path.replace('\\', "/");
    let stem = Path::new(&relative)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let folder = Path::new(&relative)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    JsonMap::from_iter([
        ("path".to_string(), JsonValue::String(relative.clone())),
        ("basename".to_string(), JsonValue::String(stem.clone())),
        ("title".to_string(), JsonValue::String(stem)),
        ("folder".to_string(), JsonValue::String(folder)),
    ])
}

fn path_to_relative_file_json(path: &Path) -> JsonMap<String, JsonValue> {
    file_object_json(&path.to_string_lossy().replace('\\', "/"))
}

#[cfg(feature = "js_runtime")]
fn resolve_vault_path_from_state(state: &JsTemplateState, identifier: &str) -> Option<String> {
    if let Ok(reference) = resolve_note_reference(&state.paths, identifier) {
        return Some(reference.path);
    }
    let trimmed = identifier
        .trim()
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .unwrap_or(identifier)
        .split('#')
        .next()
        .unwrap_or(identifier)
        .trim();
    let path = Path::new(trimmed);
    let candidate = if path.extension().is_none() {
        PathBuf::from(format!("{trimmed}.md"))
    } else {
        path.to_path_buf()
    };
    state
        .paths
        .vault_root()
        .join(&candidate)
        .is_file()
        .then(|| candidate.to_string_lossy().replace('\\', "/"))
}

#[cfg(feature = "js_runtime")]
fn ensure_allowlisted_url(config: &TemplatesConfig, url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    let Some(host) = parsed.host_str() else {
        return Err("templater web requests require a hostname".to_string());
    };
    if config
        .web_allowlist
        .iter()
        .any(|allowed| host == allowed || host.ends_with(&format!(".{allowed}")))
    {
        Ok(())
    } else {
        Err(format!(
            "templater web request host `{host}` is not allowlisted in [templates].web_allowlist"
        ))
    }
}

fn random_picture_markdown(
    size: Option<&str>,
    query: Option<&str>,
    include_size: bool,
) -> String {
    let mut url = "https://source.unsplash.com/random".to_string();
    if let Some(size) = size.filter(|value| !value.trim().is_empty()) {
        url.push('/');
        url.push_str(size);
    }
    if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    if include_size {
        size.map_or_else(|| format!("![]({url})"), |size| format!("![]({url}|{size})"))
    } else {
        format!("![]({url})")
    }
}

#[cfg(feature = "js_runtime")]
fn html_to_markdown(html: &str) -> String {
    html.replace("<strong>", "**")
        .replace("</strong>", "**")
        .replace("<b>", "**")
        .replace("</b>", "**")
        .replace("<em>", "*")
        .replace("</em>", "*")
        .replace("<i>", "*")
        .replace("</i>", "*")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<p>", "")
        .replace("</p>", "\n\n")
        .replace("<h1>", "# ")
        .replace("</h1>", "\n\n")
        .replace("<h2>", "## ")
        .replace("</h2>", "\n\n")
}

#[cfg(feature = "js_runtime")]
fn strip_top_level_await(source: &str) -> String {
    source.replace("await ", "")
}

#[cfg(test)]
mod tests {
    use super::{
        apply_right_trim, parse_native_expression, parse_template_var_bindings,
        parse_templater_tag, random_picture_markdown, render_template_request,
        template_value_to_string, TemplateEngineKind, TemplateRenderRequest, TemplateRunMode,
        TemplateValue, TrimMode,
    };
    use std::collections::HashMap;
    use tempfile::tempdir;
    use vulcan_core::{VaultConfig, VaultPaths};
    #[cfg(feature = "js_runtime")]
    use crate::TemplateCandidate;
    #[cfg(feature = "js_runtime")]
    use std::fs;
    #[cfg(feature = "js_runtime")]
    use std::io::{Read, Write};
    #[cfg(feature = "js_runtime")]
    use std::net::TcpListener;
    #[cfg(feature = "js_runtime")]
    use std::path::Path;

    #[test]
    fn parses_template_var_bindings() {
        let vars = parse_template_var_bindings(&[
            "project=Vulcan".to_string(),
            "mood=focused".to_string(),
        ])
        .expect("vars should parse");
        assert_eq!(vars["project"], "Vulcan");
        assert_eq!(vars["mood"], "focused");
    }

    #[test]
    fn detects_templater_engine_from_tag_syntax() {
        assert_eq!(
            super::detect_template_engine("<% tp.file.title %>", TemplateEngineKind::Auto),
            TemplateEngineKind::Templater
        );
        assert_eq!(
            super::detect_template_engine("{{title}}", TemplateEngineKind::Auto),
            TemplateEngineKind::Native
        );
    }

    #[test]
    fn parses_templater_tags_with_trim_markers() {
        let (tag, next) = parse_templater_tag("a<%_ tp.file.title -%>b", 1).expect("tag");
        assert_eq!(tag.left_trim, TrimMode::All);
        assert_eq!(tag.right_trim, TrimMode::Newline);
        assert_eq!(tag.body, "tp.file.title");
        assert_eq!(next, 22);
    }

    #[test]
    fn trims_one_newline_after_tag() {
        let source = "<% tp.file.title -%>\nBody";
        let cursor = apply_right_trim(source, 20, TrimMode::Newline);
        assert_eq!(&source[cursor..], "Body");
    }

    #[test]
    fn parses_native_path_and_call_expressions() {
        assert_eq!(
            parse_native_expression("tp.frontmatter[\"note type\"]").expect("path"),
            super::NativeExpression::Path(vec![
                super::NativePathPart::Name("tp".to_string()),
                super::NativePathPart::Name("frontmatter".to_string()),
                super::NativePathPart::Index("note type".to_string()),
            ])
        );
        assert!(matches!(
            parse_native_expression("tp.date.now(\"YYYY-MM-DD\", 7)").expect("call"),
            super::NativeExpression::Call { .. }
        ));
    }

    #[test]
    fn renders_arrays_like_templater() {
        assert_eq!(
            template_value_to_string(&TemplateValue::Array(vec![
                TemplateValue::String("a".to_string()),
                TemplateValue::String("b".to_string()),
                TemplateValue::String("c".to_string()),
            ])),
            "a,b,c"
        );
    }

    #[test]
    fn random_picture_supports_optional_size_markdown() {
        assert_eq!(
            random_picture_markdown(Some("200x200"), Some("landscape"), true),
            "![](https://source.unsplash.com/random/200x200?landscape|200x200)"
        );
    }

    #[test]
    fn templater_native_interpolation_reads_file_and_frontmatter_context() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let config = VaultConfig::default();
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "Title <% tp.file.title %>\nStatus <% tp.frontmatter.status %>\n",
            target_path: "Projects/Alpha.md",
            target_contents: Some("---\nstatus: active\n---\nBody\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "Title Alpha\nStatus active\n");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_js_interpolation_supports_string_methods() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let config = VaultConfig::default();
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<% tp.file.title.toUpperCase() %>",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "ALPHA");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_js_execution_uses_tr_output_accumulator() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let config = VaultConfig::default();
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<%* tR += tp.file.title + '-ok'; %>",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "Alpha-ok");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_loads_user_scripts_from_configured_folder() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join("Scripts")).expect("script dir");
        fs::write(
            temp_dir.path().join("Scripts/echo.js"),
            "module.exports = function (msg) { return `echo:${msg}`; };",
        )
        .expect("script");

        let mut config = VaultConfig::default();
        config.templates.user_scripts_folder = Some(Path::new("Scripts").to_path_buf());
        let vars = HashMap::new();
        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[TemplateCandidate {
                name: "example.md".to_string(),
                source: "vulcan",
                display_path: ".vulcan/templates/example.md".to_string(),
                absolute_path: temp_dir.path().join(".vulcan/templates/example.md"),
                warning: None,
            }],
            template_path: None,
            template_text: "<% tp.user.echo(\"Hello\") %>",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "echo:Hello");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_hooks_run_after_rendering() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let config = VaultConfig::default();
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text:
                "<%* tp.hooks.on_all_templates_executed(async () => { await tp.file.create_new('Hooked', 'Created'); }); %>Main body",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "Main body");
        assert!(rendered.changed_paths.iter().any(|path| path == "Created.md"));
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Created.md")).expect("created note"),
            "Hooked"
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_system_command_functions_expand_internal_templates() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let mut config = VaultConfig::default();
        config.templates.enable_system_commands = true;
        config.templates.templates_pairs = vec![vulcan_core::config::TemplaterCommandPairConfig {
            name: "echo".to_string(),
            command: "echo <% tp.file.title %>".to_string(),
        }];
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<% tp.user.echo() %>",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content.trim(), "Alpha");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn templater_web_requests_respect_allowlist_and_json_path() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let body = r#"{"title":"Vulcan"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should write");
        });

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let mut config = VaultConfig::default();
        config.templates.web_allowlist = vec!["127.0.0.1".to_string()];
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: &format!(
                "<% tp.web.request(\"http://127.0.0.1:{}/data\", \"title\") %>",
                address.port()
            ),
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "Vulcan");
    }

    #[cfg(not(feature = "js_runtime"))]
    #[test]
    fn templater_web_helpers_emit_diagnostics_without_js_runtime() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        let config = VaultConfig::default();
        let vars = HashMap::new();

        let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<% tp.web.request(\"https://example.com\") %>",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
        })
        .expect("template should render");

        assert_eq!(rendered.content, "");
        assert_eq!(rendered.diagnostics.len(), 1);
        assert!(rendered.diagnostics[0].contains("js_runtime"));
    }
}
