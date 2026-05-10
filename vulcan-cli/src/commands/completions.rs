use regex::Regex;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path};
use vulcan_core::expression::functions::date_components;
use vulcan_core::VaultPaths;

pub(crate) fn run_complete_command(paths: &VaultPaths, context: &str, prefix: Option<&str>) {
    let candidates = collect_complete_candidates(paths, context, prefix);
    for candidate in &candidates {
        println!("{candidate}");
    }
}

/// Candidates for contexts that require no vault (safe to call before path resolution).
pub(crate) fn collect_complete_candidates_no_vault(
    context: &str,
    prefix: Option<&str>,
) -> Vec<String> {
    let candidates = match context {
        "daily-date" => {
            let mut dates = vec![
                "today".to_string(),
                "yesterday".to_string(),
                "tomorrow".to_string(),
            ];
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            for offset in 1..=14i64 {
                let past_secs = now_secs - offset * 86400;
                let ms = past_secs * 1000;
                let (year, month, day, _, _, _, _) = date_components(ms);
                dates.push(format!("{year:04}-{month:02}-{day:02}"));
            }
            dates
        }
        _ => Vec::new(),
    };
    filter_completion_candidates(candidates, prefix)
}

fn filter_completion_candidates(mut candidates: Vec<String>, prefix: Option<&str>) -> Vec<String> {
    let Some(prefix) = prefix.filter(|value| !value.is_empty()) else {
        return candidates;
    };
    candidates.retain(|candidate| candidate.starts_with(prefix));
    candidates
}

fn collect_vault_path_candidates(paths: &VaultPaths, prefix: Option<&str>) -> Vec<String> {
    let prefix = prefix.unwrap_or_default().replace('\\', "/");
    let trimmed = prefix.trim_start_matches("./");
    let (dir_prefix, partial_name) = match trimmed.rsplit_once('/') {
        Some((directory, partial)) => (directory.trim_end_matches('/'), partial),
        None => ("", trimmed),
    };

    if Path::new(dir_prefix).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Vec::new();
    }

    let directory = if dir_prefix.is_empty() {
        paths.vault_root().to_path_buf()
    } else {
        paths.vault_root().join(dir_prefix)
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut candidates = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() {
            continue;
        }
        if dir_prefix.is_empty() && matches!(name.as_str(), ".git" | ".vulcan") {
            continue;
        }
        if !name.starts_with(partial_name) {
            continue;
        }

        let mut candidate = if dir_prefix.is_empty() {
            name
        } else {
            format!("{dir_prefix}/{name}")
        };
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            candidate.push('/');
        }
        candidates.insert(candidate.replace('\\', "/"));
    }
    candidates.into_iter().collect()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn collect_complete_candidates(
    paths: &VaultPaths,
    context: &str,
    prefix: Option<&str>,
) -> Vec<String> {
    if context != "daily-date" {
        let vault_free = collect_complete_candidates_no_vault(context, prefix);
        if !vault_free.is_empty() {
            return vault_free;
        }
    }

    let candidates = match context {
        "vault-path" => collect_vault_path_candidates(paths, prefix),
        "script" => {
            let scripts_dir = paths.vulcan_dir().join("scripts");
            if !scripts_dir.is_dir() {
                return Vec::new();
            }
            fs::read_dir(&scripts_dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|entry| {
                            let name = entry.file_name();
                            let candidate = name.to_string_lossy();
                            if candidate.ends_with(".js") {
                                Some(candidate.trim_end_matches(".js").to_string())
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        _ => vulcan_app::browse::collect_complete_candidates(paths, context).unwrap_or_default(),
    };
    if context == "vault-path" {
        candidates
    } else {
        filter_completion_candidates(candidates, prefix)
    }
}

/// `clap_complete` generates Fish nested-subcommand completions with a condition like
///   `__fish_seen_subcommand_from PARENT`
/// but never adds a `not __fish_seen_subcommand_from CHILD1 CHILD2` guard.  That
/// means that once the user types e.g. `tasks view show`, Fish still offers `show`
/// and `list` as candidates for the next word (cycling ad nauseam).
///
/// This function collects all lines that:
///   1. have a condition ending with `; and __fish_seen_subcommand_from \w+`  (no `not` after)
///   2. offer a bare word subcommand via `-f -a "word"`
///
/// groups them by condition, then re-emits those lines with the missing
/// `; and not __fish_seen_subcommand_from WORD1 WORD2 …` guard appended.
pub(crate) fn fix_fish_nested_subcommand_guards(script: &str) -> String {
    // Capture (full_line, condition_base, subcommand_word)
    // Pattern: ...-n "COND" ... -f -a "WORD" ...
    // where COND ends with `; and __fish_seen_subcommand_from \w+` (no trailing `; and not`)
    // Match lines of the form:
    //   complete -c vulcan -n "COND; and __fish_seen_subcommand_from WORD" -f -a "SUB" -d '...'
    // where the condition ends with `__fish_seen_subcommand_from WORD` (no further `; and ...`).
    // These are the nested-subcommand candidate lines that need a `not` guard.
    let line_re = Regex::new(
        r#"^(complete -c vulcan -n ")(.*; and __fish_seen_subcommand_from \w+)(" .*-f -a ")(\w+)(" .*)$"#,
    )
    .expect("regex should compile");

    let mut condition_to_words: HashMap<String, Vec<String>> = HashMap::new();
    for line in script.lines() {
        if let Some(caps) = line_re.captures(line) {
            let cond = caps[2].to_string();
            let word = caps[4].to_string();
            condition_to_words.entry(cond).or_default().push(word);
        }
    }

    // Only patch conditions that have more than one subcommand — a single-child
    // parent has nothing else to cycle through so no guard is needed.
    let mut out = String::with_capacity(script.len() + 512);
    for line in script.lines() {
        if let Some(caps) = line_re.captures(line) {
            let cond = caps[2].to_string();
            if let Some(words) = condition_to_words.get(&cond) {
                if words.len() > 1 {
                    let not_guard =
                        format!("; and not __fish_seen_subcommand_from {}", words.join(" "));
                    let patched = format!(
                        "{}{}{}{}{}{}",
                        &caps[1], &caps[2], not_guard, &caps[3], &caps[4], &caps[5]
                    );
                    out.push_str(&patched);
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !script.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

pub(crate) fn generate_dynamic_completions(shell: clap_complete::Shell) -> String {
    match shell {
        clap_complete::Shell::Fish => generate_fish_dynamic_completions(),
        clap_complete::Shell::Bash => generate_bash_dynamic_completions(),
        clap_complete::Shell::Zsh => generate_zsh_dynamic_completions(),
        _ => String::new(),
    }
}

fn shell_double_quote_literal(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => rendered.push_str("\\\\"),
            '"' => rendered.push_str("\\\""),
            '$' => rendered.push_str("\\$"),
            '`' => rendered.push_str("\\`"),
            _ => rendered.push(ch),
        }
    }
    rendered
}

pub(crate) fn completion_command_path_literal() -> String {
    std::env::current_exe().ok().map_or_else(
        || "vulcan".to_string(),
        |path| shell_double_quote_literal(&path.to_string_lossy()),
    )
}

pub(crate) fn generate_fish_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("../completions_fish.fish"))
}

fn render_dynamic_completion_template(template: &str) -> String {
    template
        .trim()
        .to_string()
        .replace("__VULCAN_CMD__", &completion_command_path_literal())
}

pub(crate) fn generate_bash_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("../completions_bash.sh"))
}

pub(crate) fn generate_zsh_dynamic_completions() -> String {
    render_dynamic_completion_template(include_str!("../completions_zsh.zsh"))
}
