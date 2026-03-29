mod ast;
mod eval;
mod parse;
mod recurrence;

use std::fmt::{Display, Formatter};

use serde::Serialize;

use crate::cache::CacheDatabase;
use crate::paths::VaultPaths;
use crate::properties::PropertyError;
use crate::resolve_note_reference;

pub use ast::{
    TasksDateField, TasksDateRelation, TasksFilter, TasksQuery, TasksQueryCommand, TasksTextField,
};
pub use eval::{
    evaluate_parsed_tasks_query, evaluate_tasks_query, TasksQueryGroup, TasksQueryResult,
};
pub use parse::parse_tasks_query;
pub(crate) use recurrence::{inject_task_recurrence_fields, task_recurrence_properties};
pub use recurrence::{
    parse_recurrence_text, parse_task_recurrence, task_recurrence_anchor,
    task_upcoming_occurrences, TaskRecurrence,
};

#[derive(Debug)]
pub enum TasksError {
    Parse(String),
    Property(PropertyError),
    Message(String),
}

impl Display for TasksError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(message) | Self::Message(message) => f.write_str(message),
            Self::Property(error) => Display::fmt(error, f),
        }
    }
}

impl std::error::Error for TasksError {}

impl From<PropertyError> for TasksError {
    fn from(error: PropertyError) -> Self {
        Self::Property(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TasksBlockRecord {
    pub file: String,
    pub block_index: usize,
    pub line_number: i64,
    pub source: String,
}

pub fn load_tasks_blocks(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
) -> Result<Vec<TasksBlockRecord>, TasksError> {
    let resolved = resolve_note_reference(paths, file)
        .map_err(|error| TasksError::Message(error.to_string()))?;
    let database =
        CacheDatabase::open(paths).map_err(|error| TasksError::Message(error.to_string()))?;
    let connection = database.connection();
    let mut statement = connection
        .prepare(
            "SELECT tasks_blocks.block_index, tasks_blocks.line_number, tasks_blocks.raw_text
             FROM tasks_blocks
             JOIN documents ON documents.id = tasks_blocks.document_id
             WHERE documents.path = ?1
             ORDER BY tasks_blocks.block_index",
        )
        .map_err(|error| TasksError::Message(error.to_string()))?;
    let rows = statement
        .query_map([resolved.path.as_str()], |row| {
            let block_index = row.get::<_, i64>(0)?;
            Ok(TasksBlockRecord {
                file: resolved.path.clone(),
                block_index: usize::try_from(block_index).unwrap_or_default(),
                line_number: row.get(1)?,
                source: row.get(2)?,
            })
        })
        .map_err(|error| TasksError::Message(error.to_string()))?;

    let mut blocks = Vec::new();
    for row in rows {
        blocks.push(row.map_err(|error| TasksError::Message(error.to_string()))?);
    }

    if let Some(requested_block) = block {
        return blocks
            .into_iter()
            .find(|candidate| candidate.block_index == requested_block)
            .map(|candidate| vec![candidate])
            .ok_or_else(|| {
                TasksError::Message(format!(
                    "no tasks block {requested_block} found in {}",
                    resolved.path
                ))
            });
    }

    if blocks.is_empty() {
        return Err(TasksError::Message(format!(
            "no tasks blocks found in {}",
            resolved.path
        )));
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::{scan_vault, ScanMode};

    use super::*;

    #[test]
    fn load_tasks_blocks_reads_indexed_query_blocks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Tasks.md"),
            concat!(
                "```tasks\n",
                "not done\n",
                "sort by due\n",
                "```\n\n",
                "```tasks\n",
                "done\n",
                "limit 3\n",
                "```\n"
            ),
        )
        .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let blocks = load_tasks_blocks(&paths, "Tasks", None).expect("tasks blocks should load");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].file, "Tasks.md");
        assert_eq!(blocks[0].block_index, 0);
        assert_eq!(blocks[0].line_number, 1);
        assert_eq!(blocks[0].source, "not done\nsort by due");
        assert_eq!(blocks[1].block_index, 1);
        assert_eq!(blocks[1].line_number, 6);
        assert_eq!(blocks[1].source, "done\nlimit 3");
    }

    #[test]
    fn load_tasks_blocks_filters_to_requested_block() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Tasks.md"),
            "```tasks\nnot done\n```\n\n```tasks\ndone\n```\n",
        )
        .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let blocks = load_tasks_blocks(&paths, "Tasks", Some(1)).expect("block should load");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_index, 1);
        assert_eq!(blocks[0].source, "done");
    }

    #[test]
    fn load_tasks_blocks_errors_when_note_has_no_tasks_blocks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(vault_root.join("Home.md"), "# Home\n").expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let error = load_tasks_blocks(&paths, "Home", None).expect_err("loading should fail");
        assert_eq!(error.to_string(), "no tasks blocks found in Home.md");
    }
}
