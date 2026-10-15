//! The routed `query` operation: a Vulcan query (DSL or JSON AST).

use serde::{Deserialize, Serialize};
use vulcan_core::note_store::NoteStore;
use vulcan_core::properties::NotePage;
use vulcan_core::{PermissionFilter, QueryAst, QueryReport, VaultPaths};

use crate::AppError;

/// A `query` with the paging the client will print. The page is part of the
/// request because only the printed page is hydrated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub ast: QueryAst,
    /// The client's `--offset`, when given (0 otherwise).
    pub offset: usize,
    /// The client's `--limit`, when given.
    pub limit: Option<usize>,
    /// Whether a filename glob filters the notes before printing.
    pub glob: bool,
}

impl QueryRequest {
    /// The notes the printer shows: the page it slices (its offset and limit,
    /// or the query's) when no glob filters the notes first. Hydrating every
    /// match for a page of 50 cost seconds at 100K notes.
    fn page(&self) -> Option<NotePage> {
        let offset = if self.offset > 0 {
            self.offset
        } else {
            self.ast.offset
        };
        printed_page(offset, self.limit.or(self.ast.limit), self.glob)
    }
}

/// The notes a printer that skips `offset` and shows `limit` notes reads,
/// unless a filename glob filters the notes before printing.
pub(crate) fn printed_page(offset: usize, limit: Option<usize>, glob: bool) -> Option<NotePage> {
    // An empty page would also empty the report `--exit-code` inspects.
    limit
        .filter(|limit| !glob && *limit > 0)
        .map(|limit| NotePage {
            offset: 0,
            limit: Some(offset.saturating_add(limit)),
        })
}

/// Run `request` over `store`: the cache directly for the CLI, a retained
/// session for a daemon.
pub fn run_query(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    request: &QueryRequest,
    filter: Option<&PermissionFilter>,
) -> Result<QueryReport, AppError> {
    vulcan_core::execute_query_report_page_in(
        store,
        paths,
        request.ast.clone(),
        filter,
        request.page(),
    )
    .map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(offset: usize, limit: Option<usize>, glob: bool, dsl: &str) -> QueryRequest {
        QueryRequest {
            ast: QueryAst::from_dsl(dsl).unwrap(),
            offset,
            limit,
            glob,
        }
    }

    #[test]
    fn the_page_covers_what_the_printer_shows() {
        let page = |request: QueryRequest| request.page();
        assert_eq!(page(request(0, None, false, "from notes")), None);
        assert_eq!(
            page(request(0, None, false, "from notes limit 10")),
            Some(NotePage {
                offset: 0,
                limit: Some(10)
            })
        );
        assert_eq!(
            page(request(5, Some(10), false, "from notes limit 3")),
            Some(NotePage {
                offset: 0,
                limit: Some(15)
            })
        );
        // A glob filters before printing, and a zero limit keeps every match.
        assert_eq!(page(request(0, Some(10), true, "from notes")), None);
        assert_eq!(page(request(0, Some(0), false, "from notes")), None);
    }
}
