//! Versioned deterministic merge-policy contract.

use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};

pub const MERGE_POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_RULES: usize = 64;
const MAX_COMPONENT_BYTES: usize = 512;

/// A device-local ceiling may require review but never selects a different tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergeAutomation {
    #[default]
    AllowPolicy,
    RequireReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeFileKind {
    Markdown,
    Json,
    Canvas,
    Bases,
    ObsidianState,
    Text,
    Binary,
    Missing,
}

impl MergeFileKind {
    #[must_use]
    pub fn classify(path: &str, sides: &[Option<&[u8]>]) -> Self {
        let present = sides.iter().flatten().copied().collect::<Vec<_>>();
        if present.is_empty() {
            return Self::Missing;
        }
        if present
            .iter()
            .any(|data| data.contains(&0) || std::str::from_utf8(data).is_err())
        {
            return Self::Binary;
        }
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str());
        if path
            .get(..".obsidian/".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(".obsidian/"))
        {
            Self::ObsidianState
        } else if extension.is_some_and(|extension| extension.eq_ignore_ascii_case("canvas")) {
            Self::Canvas
        } else if extension.is_some_and(|extension| extension.eq_ignore_ascii_case("base")) {
            Self::Bases
        } else if extension.is_some_and(|extension| extension.eq_ignore_ascii_case("json")) {
            Self::Json
        } else if extension
            .is_some_and(|extension| matches_ignore_ascii_case(extension, &["md", "markdown"]))
        {
            Self::Markdown
        } else {
            Self::Text
        }
    }
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePathSelector {
    pub glob: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<MergeFileKind>,
}

impl MergePathSelector {
    fn matches(&self, path: &str, kind: MergeFileKind) -> Result<bool, MergePolicyError> {
        let matcher = GlobBuilder::new(&self.glob)
            .literal_separator(true)
            .case_insensitive(false)
            .build()
            .map_err(|error| MergePolicyError::InvalidRule(error.to_string()))?
            .compile_matcher();
        Ok(matcher.is_match(path) && (self.kinds.is_empty() || self.kinds.contains(&kind)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeResolution {
    Structured,
    RequireReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePolicyRule {
    pub id: String,
    pub selector: MergePathSelector,
    pub resolution: MergeResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePolicy {
    pub version: u32,
    pub rules: Vec<MergePolicyRule>,
}

impl Default for MergePolicy {
    fn default() -> Self {
        Self {
            version: MERGE_POLICY_SCHEMA_VERSION,
            rules: vec![
                rule(
                    "obsidian-device-state-review",
                    ".obsidian/**",
                    &[MergeFileKind::ObsidianState],
                    MergeResolution::RequireReview,
                ),
                rule(
                    "canvas-structured",
                    "**/*.canvas",
                    &[MergeFileKind::Canvas],
                    MergeResolution::Structured,
                ),
                rule(
                    "bases-structured",
                    "**/*.base",
                    &[MergeFileKind::Bases],
                    MergeResolution::Structured,
                ),
                rule(
                    "json-structured",
                    "**/*.json",
                    &[MergeFileKind::Json],
                    MergeResolution::Structured,
                ),
                rule(
                    "markdown-structured",
                    "**/*.md",
                    &[MergeFileKind::Markdown],
                    MergeResolution::Structured,
                ),
                rule(
                    "markdown-long-extension-structured",
                    "**/*.markdown",
                    &[MergeFileKind::Markdown],
                    MergeResolution::Structured,
                ),
                rule("fallback-review", "**", &[], MergeResolution::RequireReview),
            ],
        }
    }
}

impl MergePolicy {
    pub fn validate(&self) -> Result<(), MergePolicyError> {
        if self.version != MERGE_POLICY_SCHEMA_VERSION {
            return Err(MergePolicyError::UnsupportedVersion(self.version));
        }
        if self.rules.is_empty() || self.rules.len() > MAX_RULES {
            return Err(MergePolicyError::InvalidRule(format!(
                "merge policy must contain 1-{MAX_RULES} ordered rules"
            )));
        }
        for rule in &self.rules {
            if !valid_component(&rule.id) || !valid_component(&rule.selector.glob) {
                return Err(MergePolicyError::InvalidRule(
                    "merge rule IDs and globs must be bounded non-control strings".to_string(),
                ));
            }
            rule.selector
                .matches("validation/path.md", MergeFileKind::Markdown)?;
        }
        Ok(())
    }

    pub fn resolution_for(
        &self,
        path: &str,
        kind: MergeFileKind,
        automation: MergeAutomation,
    ) -> Result<MergeResolution, MergePolicyError> {
        self.validate()?;
        if automation == MergeAutomation::RequireReview {
            return Ok(MergeResolution::RequireReview);
        }
        for rule in &self.rules {
            if rule.selector.matches(path, kind)? {
                return Ok(rule.resolution);
            }
        }
        Err(MergePolicyError::InvalidRule(format!(
            "merge policy has no matching rule for `{path}`"
        )))
    }

    pub fn policy_hash(&self) -> Result<String, MergePolicyError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| MergePolicyError::InvalidRule(error.to_string()))?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

fn rule(
    id: &str,
    glob: &str,
    kinds: &[MergeFileKind],
    resolution: MergeResolution,
) -> MergePolicyRule {
    MergePolicyRule {
        id: id.to_string(),
        selector: MergePathSelector {
            glob: glob.to_string(),
            kinds: kinds.to_vec(),
        },
        resolution,
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_COMPONENT_BYTES
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergePolicyError {
    UnsupportedVersion(u32),
    InvalidRule(String),
}

impl Display for MergePolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported merge policy version {version}")
            }
            Self::InvalidRule(detail) => formatter.write_str(detail),
        }
    }
}

impl Error for MergePolicyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_fixed_ordered_and_have_a_stable_hash() {
        let policy = MergePolicy::default();
        policy.validate().expect("default policy");
        assert_eq!(policy.version, 1);
        assert_eq!(
            policy.rules.first().expect("first").id,
            "obsidian-device-state-review"
        );
        assert_eq!(policy.rules.last().expect("last").id, "fallback-review");
        let first = policy.policy_hash().expect("hash");
        let second: MergePolicy =
            serde_json::from_value(serde_json::to_value(&policy).expect("serialize"))
                .expect("deserialize");
        assert_eq!(second.policy_hash().expect("hash"), first);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn ordered_rules_and_local_ceiling_never_increase_automation() {
        let policy = MergePolicy::default();
        assert_eq!(
            policy
                .resolution_for(
                    ".obsidian/workspace.json",
                    MergeFileKind::ObsidianState,
                    MergeAutomation::AllowPolicy,
                )
                .expect("resolution"),
            MergeResolution::RequireReview
        );
        assert_eq!(
            policy
                .resolution_for(
                    "Notes/Home.md",
                    MergeFileKind::Markdown,
                    MergeAutomation::AllowPolicy,
                )
                .expect("resolution"),
            MergeResolution::Structured
        );
        assert_eq!(
            policy
                .resolution_for(
                    "Notes/Home.md",
                    MergeFileKind::Markdown,
                    MergeAutomation::RequireReview,
                )
                .expect("resolution"),
            MergeResolution::RequireReview
        );
    }

    #[test]
    fn file_kind_classification_is_content_aware() {
        assert_eq!(
            MergeFileKind::classify("Home.md", &[Some(b"# Home")]),
            MergeFileKind::Markdown
        );
        assert_eq!(
            MergeFileKind::classify("Home.md", &[Some(b"a\0b")]),
            MergeFileKind::Binary
        );
        assert_eq!(
            MergeFileKind::classify("gone.md", &[None, None, None]),
            MergeFileKind::Missing
        );
        assert_eq!(
            serde_json::to_value(MergeAutomation::RequireReview).expect("serialize"),
            json!("require_review")
        );
    }

    #[test]
    fn malformed_versions_globs_and_unmatched_policies_fail_closed() {
        let policy = MergePolicy {
            version: 2,
            ..MergePolicy::default()
        };
        assert!(matches!(
            policy.validate(),
            Err(MergePolicyError::UnsupportedVersion(2))
        ));
        let policy = MergePolicy {
            version: 1,
            rules: vec![rule("broken", "[", &[], MergeResolution::Structured)],
        };
        assert!(policy.validate().is_err());
        let policy = MergePolicy {
            version: 1,
            rules: vec![rule(
                "markdown-only",
                "**/*.md",
                &[],
                MergeResolution::Structured,
            )],
        };
        assert!(policy
            .resolution_for(
                "data.json",
                MergeFileKind::Json,
                MergeAutomation::AllowPolicy
            )
            .is_err());
    }
}
