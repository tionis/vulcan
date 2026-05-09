#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ExternalUserPrincipal(String);

impl ExternalUserPrincipal {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, ChatTransportError> {
        let value = value.into();
        validate_prefixed_identifier(&value, "external user principal")?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ExternalUserPrincipal {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ChatSpaceId(String);

impl ChatSpaceId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, ChatTransportError> {
        let value = value.into();
        validate_prefixed_identifier(&value, "chat space id")?;
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ChatSpaceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatSpace {
    pub(crate) id: ChatSpaceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parent_space_id: Option<ChatSpaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IdentityBinding {
    pub(crate) external_user: ExternalUserPrincipal,
    pub(crate) vault_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auth_principal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AdapterCapabilities {
    pub(crate) reactions: bool,
    pub(crate) message_edits: bool,
    pub(crate) replies: bool,
    pub(crate) buttons: bool,
    pub(crate) attachments: bool,
    pub(crate) threads: bool,
    pub(crate) ephemeral_messages: bool,
}

impl AdapterCapabilities {
    pub(crate) fn telegram_like() -> Self {
        Self {
            reactions: true,
            message_edits: true,
            replies: true,
            buttons: true,
            attachments: true,
            threads: false,
            ephemeral_messages: false,
        }
    }

    pub(crate) fn matrix_like() -> Self {
        Self {
            reactions: true,
            message_edits: true,
            replies: true,
            buttons: false,
            attachments: true,
            threads: true,
            ephemeral_messages: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatEvent {
    Message {
        id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        attachments: Vec<ChatAttachment>,
    },
    ReactionAdded {
        message_id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
        reaction: String,
    },
    ReactionRemoved {
        message_id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
        reaction: String,
    },
    MessageEdited {
        id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
        text: String,
    },
    MessageDeleted {
        id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
    },
    Interaction {
        id: String,
        space: ChatSpaceId,
        user: ExternalUserPrincipal,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatAttachment {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatAction {
    SendMessage {
        space: ChatSpaceId,
        text: String,
        buttons: Vec<ChatButton>,
    },
    EditMessage {
        space: ChatSpaceId,
        message_id: String,
        text: String,
    },
    Reply {
        space: ChatSpaceId,
        message_id: String,
        text: String,
    },
    AddReaction {
        space: ChatSpaceId,
        message_id: String,
        reaction: String,
    },
    RemoveReaction {
        space: ChatSpaceId,
        message_id: String,
        reaction: String,
    },
    AcknowledgeInteraction {
        interaction_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatButton {
    pub(crate) label: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionSource {
    PlatformDefault,
    SpaceHierarchy,
    ExternalUserOverride,
    BoundIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionProfileCandidate {
    pub(crate) source: PermissionSource,
    pub(crate) profile: String,
    pub(crate) rank: u8,
}

pub(crate) fn resolve_restrictive_profile(
    candidates: &[PermissionProfileCandidate],
) -> Option<&str> {
    candidates
        .iter()
        .min_by_key(|candidate| candidate.rank)
        .map(|candidate| candidate.profile.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatTransportError {
    message: String,
}

impl ChatTransportError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ChatTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ChatTransportError {}

fn validate_prefixed_identifier(value: &str, label: &str) -> Result<(), ChatTransportError> {
    if value.trim() != value || value.is_empty() {
        return Err(ChatTransportError::new(format!(
            "{label} must not be empty"
        )));
    }
    let Some((scheme, rest)) = value.split_once(':') else {
        return Err(ChatTransportError::new(format!(
            "{label} `{value}` must include a platform prefix"
        )));
    };
    if scheme.is_empty() || rest.is_empty() {
        return Err(ChatTransportError::new(format!(
            "{label} `{value}` must include both prefix and identifier"
        )));
    }
    if !scheme
        .chars()
        .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        return Err(ChatTransportError::new(format!(
            "{label} `{value}` has an invalid prefix"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_require_platform_prefixes() {
        assert_eq!(
            ExternalUserPrincipal::new("telegram:123456")
                .expect("principal should parse")
                .as_str(),
            "telegram:123456"
        );
        assert!(ExternalUserPrincipal::new("alice").is_err());
        assert!(ChatSpaceId::new("discord:guild/123/channel/456/thread/789").is_ok());
        assert!(ChatSpaceId::new("Matrix:@alice:example.com").is_err());
    }

    #[test]
    fn chat_events_round_trip_as_tagged_json() {
        let event = ChatEvent::Message {
            id: "m1".to_string(),
            space: ChatSpaceId::new("matrix:!roomid:example.com").expect("space should parse"),
            user: ExternalUserPrincipal::new("matrix:@alice:example.com")
                .expect("user should parse"),
            text: "hello".to_string(),
            reply_to: Some("m0".to_string()),
            attachments: vec![ChatAttachment {
                id: "a1".to_string(),
                name: "note.md".to_string(),
                mime_type: Some("text/markdown".to_string()),
            }],
        };

        let value = serde_json::to_value(&event).expect("event should serialize");
        assert_eq!(value["type"].as_str(), Some("message"));
        assert_eq!(value["space"].as_str(), Some("matrix:!roomid:example.com"));
        let reparsed: ChatEvent = serde_json::from_value(value).expect("event should parse");
        assert_eq!(reparsed, event);
    }

    #[test]
    fn restrictive_profile_picks_lowest_ranked_candidate() {
        let candidates = vec![
            PermissionProfileCandidate {
                source: PermissionSource::PlatformDefault,
                profile: "readonly".to_string(),
                rank: 10,
            },
            PermissionProfileCandidate {
                source: PermissionSource::ExternalUserOverride,
                profile: "edit".to_string(),
                rank: 20,
            },
            PermissionProfileCandidate {
                source: PermissionSource::SpaceHierarchy,
                profile: "daily-wiki-agent".to_string(),
                rank: 5,
            },
        ];

        assert_eq!(
            resolve_restrictive_profile(&candidates),
            Some("daily-wiki-agent")
        );
    }
}
