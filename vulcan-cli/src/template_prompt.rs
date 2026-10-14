//! Terminal answers for Templater `tp.system.prompt()` and suggesters.
//!
//! Prompts are line based on stderr/stdin so they work in any terminal and
//! never mix with command output on stdout.

use std::io::{self, BufRead, IsTerminal, Write};
use vulcan_app::templates::{TemplatePromptRequest, TemplatePrompter, TemplateSuggestRequest};

/// Whether this process can ask the user: both stdin and stderr must be a
/// terminal, since prompts are written to stderr and answers read from stdin.
pub(crate) fn interactive_prompts_available() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub(crate) struct TerminalTemplatePrompter;

impl TemplatePrompter for TerminalTemplatePrompter {
    fn prompt(&self, request: &TemplatePromptRequest) -> Result<Option<String>, String> {
        let mut stderr = io::stderr().lock();
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let label = if request.message.trim().is_empty() {
            "Value"
        } else {
            request.message.trim()
        };
        if request.multiline {
            writeln!(
                stderr,
                "{label}{} (finish with a line containing only `.`):",
                default_hint(request.default.as_deref())
            )
            .map_err(|error| error.to_string())?;
            let mut lines = Vec::new();
            loop {
                let Some(line) = read_line(&mut input)? else {
                    if lines.is_empty() {
                        return Ok(None);
                    }
                    break;
                };
                if line == "." {
                    break;
                }
                lines.push(line);
            }
            let answer = lines.join("\n");
            return Ok(Some(apply_default(answer, request.default.as_deref())));
        }
        write!(
            stderr,
            "{label}{}: ",
            default_hint(request.default.as_deref())
        )
        .map_err(|error| error.to_string())?;
        stderr.flush().map_err(|error| error.to_string())?;
        Ok(read_line(&mut input)?.map(|answer| apply_default(answer, request.default.as_deref())))
    }

    fn suggest(&self, request: &TemplateSuggestRequest) -> Result<Option<Vec<usize>>, String> {
        let mut stderr = io::stderr().lock();
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let title = if request.placeholder.trim().is_empty() {
            if request.multiple {
                "Choose one or more"
            } else {
                "Choose one"
            }
        } else {
            request.placeholder.trim()
        };
        let mut visible: Vec<usize> = (0..request.options.len()).collect();
        loop {
            writeln!(stderr, "{title}:").map_err(|error| error.to_string())?;
            let shown = request.limit.unwrap_or(usize::MAX).max(1);
            for index in visible.iter().take(shown) {
                writeln!(stderr, "  {:>2}) {}", index + 1, request.options[*index])
                    .map_err(|error| error.to_string())?;
            }
            if visible.len() > shown {
                writeln!(stderr, "  … {} more; type to filter", visible.len() - shown)
                    .map_err(|error| error.to_string())?;
            }
            write!(
                stderr,
                "{} ",
                if request.multiple {
                    "Numbers (e.g. 1,3) or text to filter:"
                } else {
                    "Number or text to filter:"
                }
            )
            .map_err(|error| error.to_string())?;
            stderr.flush().map_err(|error| error.to_string())?;
            let Some(line) = read_line(&mut input)? else {
                writeln!(stderr).map_err(|error| error.to_string())?;
                return Ok(None);
            };
            match parse_selection(&line, &request.options, &visible, request.multiple) {
                Selection::Chosen(indices) => return Ok(Some(indices)),
                Selection::Filtered(indices) => visible = indices,
                Selection::Retry(message) => {
                    writeln!(stderr, "{message}").map_err(|error| error.to_string())?;
                }
            }
        }
    }
}

fn read_line(input: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let read = input
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
}

fn default_hint(default: Option<&str>) -> String {
    default
        .filter(|default| !default.is_empty())
        .map(|default| format!(" [{default}]"))
        .unwrap_or_default()
}

fn apply_default(answer: String, default: Option<&str>) -> String {
    match default {
        Some(default) if answer.is_empty() => default.to_string(),
        _ => answer,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    Chosen(Vec<usize>),
    /// Narrow the list to these option indices and ask again.
    Filtered(Vec<usize>),
    Retry(String),
}

/// Interprets one answer line: option numbers pick directly, anything else
/// filters the visible options and picks when exactly one remains.
fn parse_selection(line: &str, options: &[String], visible: &[usize], multiple: bool) -> Selection {
    let line = line.trim();
    if line.is_empty() {
        return if visible.len() == 1 {
            Selection::Chosen(visible.to_vec())
        } else {
            Selection::Filtered((0..options.len()).collect())
        };
    }
    let parts: Vec<&str> = if multiple {
        line.split([',', ' '])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect()
    } else {
        vec![line]
    };
    if parts.iter().all(|part| part.parse::<usize>().is_ok()) {
        let mut indices = Vec::new();
        for part in parts {
            let number = part.parse::<usize>().unwrap_or_default();
            if number == 0 || number > options.len() {
                return Selection::Retry(format!(
                    "{number} is not an option; choose 1-{}",
                    options.len()
                ));
            }
            if !indices.contains(&(number - 1)) {
                indices.push(number - 1);
            }
        }
        return Selection::Chosen(indices);
    }
    if let Some(index) = options.iter().position(|option| option == line) {
        return Selection::Chosen(vec![index]);
    }
    let needle = line.to_lowercase();
    let matches: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|index| options[*index].to_lowercase().contains(&needle))
        .collect();
    match matches.len() {
        0 => Selection::Retry(format!("no option matches {line:?}")),
        1 if !multiple => Selection::Chosen(matches),
        _ => Selection::Filtered(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_selection, Selection};

    fn options() -> Vec<String> {
        ["Meeting", "Idea", "Ideal week"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn numbers_choose_options_directly() {
        let options = options();
        let all = [0, 1, 2];
        assert_eq!(
            parse_selection("2", &options, &all, false),
            Selection::Chosen(vec![1])
        );
        assert_eq!(
            parse_selection("3, 1 3", &options, &all, true),
            Selection::Chosen(vec![2, 0])
        );
        assert_eq!(
            parse_selection("4", &options, &all, false),
            Selection::Retry("4 is not an option; choose 1-3".to_string())
        );
    }

    #[test]
    fn text_filters_until_one_option_remains() {
        let options = options();
        let all = [0, 1, 2];
        assert_eq!(
            parse_selection("idea", &options, &all, false),
            Selection::Filtered(vec![1, 2])
        );
        assert_eq!(
            parse_selection("Idea", &options, &all, false),
            Selection::Chosen(vec![1])
        );
        assert_eq!(
            parse_selection("meet", &options, &all, false),
            Selection::Chosen(vec![0])
        );
        assert_eq!(
            parse_selection("", &options, &[2], false),
            Selection::Chosen(vec![2])
        );
        assert_eq!(
            parse_selection("zzz", &options, &all, false),
            Selection::Retry("no option matches \"zzz\"".to_string())
        );
    }
}
