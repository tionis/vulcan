//! The agent skills Vulcan ships, shared by `vulcan agent install` and the
//! MCP server, and the rendering that tailors a skill to where it is read.
//!
//! A bundled skill may hold target blocks: text between
//! `<!-- vulcan:cli -->` and `<!-- /vulcan:cli -->` is for agents with a
//! shell, text between `<!-- vulcan:mcp -->` and `<!-- /vulcan:mcp -->` for
//! agents that reach Vulcan through MCP tools. Text outside blocks is shared.

/// One file of a bundled skill, at its path under the skills folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledSkillFile {
    pub path: &'static str,
    pub contents: &'static str,
}

pub const BUNDLED_SKILL_FILES: &[BundledSkillFile] = &[
    BundledSkillFile {
        path: "note-operations/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/note-operations.md"),
    },
    BundledSkillFile {
        path: "vault-query/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/vault-query.md"),
    },
    BundledSkillFile {
        path: "js-api-guide/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/js-api-guide.md"),
    },
    BundledSkillFile {
        path: "skill-creator/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/skill-creator.md"),
    },
    BundledSkillFile {
        path: "graph-exploration/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/graph-exploration.md"),
    },
    BundledSkillFile {
        path: "link-curation/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/link-curation.md"),
    },
    BundledSkillFile {
        path: "daily-notes/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/daily-notes.md"),
    },
    BundledSkillFile {
        path: "properties-and-tags/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/properties-and-tags.md"),
    },
    BundledSkillFile {
        path: "refactoring/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/refactoring.md"),
    },
    BundledSkillFile {
        path: "artifact-import/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/artifact-import.md"),
    },
    BundledSkillFile {
        path: "portable-exchange/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/portable-exchange.md"),
    },
    BundledSkillFile {
        path: "web-research/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/web-research.md"),
    },
    BundledSkillFile {
        path: "git-workflow/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/git-workflow.md"),
    },
    BundledSkillFile {
        path: "sync-workflow/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/sync-workflow.md"),
    },
    BundledSkillFile {
        path: "task-management/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/task-management.md"),
    },
    BundledSkillFile {
        path: "configuration-and-permissions/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/configuration-and-permissions.md"),
    },
    BundledSkillFile {
        path: "mcp-setup/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/mcp-setup.md"),
    },
    BundledSkillFile {
        path: "editor-integration/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/editor-integration.md"),
    },
    BundledSkillFile {
        path: "index-maintenance/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/index-maintenance.md"),
    },
    BundledSkillFile {
        path: "dataview-and-bases/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/dataview-and-bases.md"),
    },
    BundledSkillFile {
        path: "templates-and-capture/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/templates-and-capture.md"),
    },
    BundledSkillFile {
        path: "mdbase-collections/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/mdbase-collections.md"),
    },
    BundledSkillFile {
        path: "publishing-and-export/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/publishing-and-export.md"),
    },
    BundledSkillFile {
        path: "plugin-authoring/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/plugin-authoring.md"),
    },
    BundledSkillFile {
        path: "diagnostics-and-repair/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/diagnostics-and-repair.md"),
    },
    BundledSkillFile {
        path: "conversation-export/SKILL.md",
        contents: include_str!("../../docs/assistant/skills/conversation-export.md"),
    },
    BundledSkillFile {
        path: "conversation-export/scripts/export-conversation.js",
        contents: include_str!(
            "../../docs/assistant/skills/conversation-export/export-conversation.js"
        ),
    },
];

/// Where a skill will be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTarget {
    Cli,
    Mcp,
}

impl SkillTarget {
    const fn marker(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

/// Keep `target`'s blocks (without their markers) and drop the other
/// target's, so each reader sees one coherent skill.
#[must_use]
pub fn render_skill(text: &str, target: SkillTarget) -> String {
    let mut rendered = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!-- vulcan:") {
        rendered.push_str(&rest[..start]);
        let after = &rest[start + "<!-- vulcan:".len()..];
        let Some(name_end) = after.find(" -->") else {
            rendered.push_str(&rest[start..]);
            return tidy(&rendered);
        };
        let name = &after[..name_end];
        let body_start = &after[name_end + " -->".len()..];
        let closing = format!("<!-- /vulcan:{name} -->");
        let Some(body_end) = body_start.find(&closing) else {
            rendered.push_str(&rest[start..]);
            return tidy(&rendered);
        };
        if name == target.marker() {
            rendered.push_str(body_start[..body_end].trim_matches('\n'));
        }
        rest = &body_start[body_end + closing.len()..];
    }
    rendered.push_str(rest);
    tidy(&rendered)
}

/// Whether a skill has a rendering for `target` beyond its shared text.
#[must_use]
pub fn has_target_blocks(text: &str, target: SkillTarget) -> bool {
    text.contains(&format!("<!-- vulcan:{} -->", target.marker()))
}

fn tidy(text: &str) -> String {
    let mut tidy = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        tidy.push_str(line);
    }
    tidy
}

/// The bundled files of one skill, by skill name.
#[must_use]
pub fn bundled_skill(name: &str) -> Vec<BundledSkillFile> {
    BUNDLED_SKILL_FILES
        .iter()
        .copied()
        .filter(|file| file.path.split('/').next() == Some(name))
        .collect()
}

/// The names of the bundled skills, in table order.
#[must_use]
pub fn bundled_skill_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for file in BUNDLED_SKILL_FILES {
        let name = file.path.split('/').next().unwrap_or_default();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_keeps_one_target_and_the_shared_text() {
        let text = "Intro.\n\n<!-- vulcan:cli -->\nRun `vulcan note get`.\n<!-- /vulcan:cli -->\n<!-- vulcan:mcp -->\nCall `note_get`.\n<!-- /vulcan:mcp -->\n\nShared end.\n";
        assert_eq!(
            render_skill(text, SkillTarget::Cli),
            "Intro.\n\nRun `vulcan note get`.\n\nShared end.\n"
        );
        assert_eq!(
            render_skill(text, SkillTarget::Mcp),
            "Intro.\n\nCall `note_get`.\n\nShared end.\n"
        );
        let inline = "Use <!-- vulcan:cli -->`vulcan search`<!-- /vulcan:cli --><!-- vulcan:mcp -->`search`<!-- /vulcan:mcp --> first.";
        assert_eq!(
            render_skill(inline, SkillTarget::Mcp),
            "Use `search` first."
        );
        assert_eq!(
            render_skill("No blocks.\n", SkillTarget::Mcp),
            "No blocks.\n"
        );
    }

    #[test]
    fn every_bundled_skill_renders_cleanly_for_both_targets() {
        for name in bundled_skill_names() {
            let skill = bundled_skill(name)
                .into_iter()
                .find(|file| file.path.ends_with("/SKILL.md"))
                .unwrap_or_else(|| panic!("{name} has a SKILL.md"));
            for target in [SkillTarget::Cli, SkillTarget::Mcp] {
                let rendered = render_skill(skill.contents, target);
                assert!(!rendered.contains("<!-- vulcan:"), "{name}: {target:?}");
                assert!(!rendered.contains("<!-- /vulcan:"), "{name}: {target:?}");
                assert!(rendered.starts_with("---\n"), "{name}: {target:?}");
            }
        }
    }
}
