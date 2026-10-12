//! Editor-facing text semantics shared by language servers and editors.
//!
//! These helpers are transport-neutral: they convert between byte offsets
//! and editor positions, find the element under a cursor, and classify what
//! is being typed. Protocol adapters such as `vulcan lsp` build on them.

mod element;
mod line_index;
mod resolution;

pub use element::{
    block_id_range, completion_context, element_at, heading_range, is_frontmatter_link, link_range,
    tag_range, CompletionContext, EditorElement,
};
pub use line_index::{LineIndex, PositionEncoding, TextPosition};
pub use resolution::{
    load_vault_identities, store_clock_version, EditorResolver, OverlayIdentity, VaultIdentities,
};
