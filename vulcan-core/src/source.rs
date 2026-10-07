//! Shared note sources (QRY.2, `docs/specs/query-architecture.md` §4.4).
//!
//! A source selects the notes a query starts from: folders, exact paths,
//! tags, and link neighbourhoods, combined with `and`/`or`/`not`. Sources
//! are exact, so they run entirely in SQL; filters then narrow the
//! candidates (§4.7). Frontends resolve their own syntax (DQL `FROM`, tag
//! and folder filters) into a [`SourceExpr`] before rendering it.

use rusqlite::types::Value as SqlValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceExpr {
    /// Notes tagged `tag` or a tag nested under it (`tag/...`). `tag` has no
    /// leading `#`; matching is byte-exact.
    Tag(String),
    /// Notes anywhere under `folder` (no trailing `/`); a byte-exact path
    /// prefix, so `%`, `_`, and case are literal. The empty folder is the
    /// whole vault.
    Folder(String),
    /// The note at exactly this vault-relative path.
    Path(String),
    /// Notes with a resolved link to the document with this id.
    LinksTo(String),
    /// Documents that the document with this id links to.
    LinkedFrom(String),
    And(Vec<SourceExpr>),
    Or(Vec<SourceExpr>),
    /// Every note the query can see except the inner source's notes.
    Not(Box<SourceExpr>),
}

/// Columns of the row a source is rendered against.
pub(crate) struct SourceColumns<'a> {
    pub id: &'a str,
    pub path: &'a str,
}

impl SourceColumns<'static> {
    pub(crate) const NOTE_QUERY: Self = Self {
        id: "note_query.document_id",
        path: "note_query.path",
    };
}

impl SourceExpr {
    /// Render a SQL boolean that is true exactly for the source's documents.
    /// It never evaluates to NULL, so `NOT` is exact. Bindings are plain `?`
    /// placeholders pushed in textual order.
    pub(crate) fn render_sql(
        &self,
        columns: &SourceColumns<'_>,
        params: &mut Vec<SqlValue>,
    ) -> String {
        match self {
            Self::Tag(tag) => {
                // `tag/` <= t < `tag0` is every `tag/...` under BINARY
                // collation, since `0` follows `/`; the range uses the index.
                params.push(SqlValue::Text(tag.clone()));
                params.push(SqlValue::Text(format!("{tag}/")));
                params.push(SqlValue::Text(format!("{tag}0")));
                // Driven by the tag index rather than probed per row;
                // `tags.document_id` is NOT NULL, so `IN` is never NULL.
                format!(
                    "{id} IN (SELECT tags.document_id FROM tags \
                     WHERE tags.tag_text = ? OR (tags.tag_text >= ? AND tags.tag_text < ?))",
                    id = columns.id
                )
            }
            Self::Folder(folder) if folder.is_empty() => "1".to_string(),
            Self::Folder(folder) => {
                params.push(SqlValue::Text(format!("{folder}/")));
                params.push(SqlValue::Text(format!("{folder}0")));
                format!("({path} >= ? AND {path} < ?)", path = columns.path)
            }
            Self::Path(path) => {
                params.push(SqlValue::Text(path.clone()));
                format!("({} = ?)", columns.path)
            }
            Self::LinksTo(target_id) => {
                params.push(SqlValue::Text(target_id.clone()));
                format!(
                    "EXISTS (SELECT 1 FROM links WHERE links.source_document_id = {} \
                     AND links.resolved_target_id = ?)",
                    columns.id
                )
            }
            Self::LinkedFrom(source_id) => {
                params.push(SqlValue::Text(source_id.clone()));
                format!(
                    "EXISTS (SELECT 1 FROM links WHERE links.resolved_target_id = {} \
                     AND links.source_document_id = ?)",
                    columns.id
                )
            }
            Self::And(children) => Self::join(children, " AND ", "1", columns, params),
            Self::Or(children) => Self::join(children, " OR ", "0", columns, params),
            Self::Not(inner) => format!("(NOT {})", inner.render_sql(columns, params)),
        }
    }

    fn join(
        children: &[Self],
        separator: &str,
        empty: &str,
        columns: &SourceColumns<'_>,
        params: &mut Vec<SqlValue>,
    ) -> String {
        if children.is_empty() {
            return empty.to_string();
        }
        let clauses = children
            .iter()
            .map(|child| child.render_sql(columns, params))
            .collect::<Vec<_>>();
        format!("({})", clauses.join(separator))
    }
}

#[cfg(test)]
mod tests;
