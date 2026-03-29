use std::fmt::{Display, Formatter};
use std::path::Path;

use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::cache::CacheDatabase;
use crate::extract_indexed_properties;
use crate::parse_document;
use crate::paths::VaultPaths;
use crate::resolve_note_reference;
use crate::{ParsedDocument, VaultConfig};

const KANBAN_FRONTMATTER_KEY: &str = "kanban-plugin";
const DATE_TRIGGER_KEY: &str = "date-trigger";
const TIME_TRIGGER_KEY: &str = "time-trigger";
const METADATA_KEYS_KEY: &str = "metadata-keys";
const ARCHIVE_WITH_DATE_KEY: &str = "archive-with-date";
const NEW_CARD_INSERTION_METHOD_KEY: &str = "new-card-insertion-method";
const SETTINGS_FOOTER_MARKER: &str = "%% kanban:settings";

#[derive(Debug)]
pub enum KanbanError {
    Message(String),
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
}

impl Display for KanbanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::Sqlite(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for KanbanError {}

impl From<rusqlite::Error> for KanbanError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<serde_json::Error> for KanbanError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedKanbanBoard {
    pub format: String,
    pub settings: Map<String, Value>,
    pub date_trigger: String,
    pub time_trigger: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KanbanBoardSummary {
    pub path: String,
    pub title: String,
    pub format: String,
    pub column_count: usize,
    pub card_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KanbanBoardRecord {
    pub path: String,
    pub title: String,
    pub format: String,
    pub date_trigger: String,
    pub time_trigger: String,
    pub settings: Value,
    pub columns: Vec<KanbanColumnRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KanbanColumnRecord {
    pub name: String,
    pub level: u8,
    pub card_count: usize,
    pub cards: Vec<KanbanCardRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KanbanCardRecord {
    pub id: String,
    pub text: String,
    pub line_number: i64,
    pub block_id: Option<String>,
    pub symbol: String,
    pub tags: Vec<String>,
    pub outlinks: Vec<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    pub inline_fields: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<KanbanTaskStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KanbanTaskStatus {
    pub status_char: String,
    pub status_name: String,
    pub status_type: String,
    pub checked: bool,
    pub completed: bool,
}

#[derive(Debug, Clone)]
struct BoardRow {
    document_id: String,
    path: String,
    format: String,
    date_trigger: String,
    time_trigger: String,
    settings: Value,
}

#[derive(Debug, Clone)]
struct HeadingRow {
    level: u8,
    text: String,
    byte_offset: i64,
}

#[derive(Debug, Clone)]
struct CardRow {
    id: String,
    text: String,
    tags: Vec<String>,
    outlinks: Vec<String>,
    line_number: i64,
    block_id: Option<String>,
    symbol: String,
    task: Option<KanbanTaskStatus>,
}

#[must_use]
pub(crate) fn extract_indexed_board(
    parsed: &ParsedDocument,
    source: &str,
    config: &VaultConfig,
) -> Option<IndexedKanbanBoard> {
    let settings = merged_board_settings(parsed.frontmatter.as_ref(), source)?;
    let format = settings
        .get(KANBAN_FRONTMATTER_KEY)
        .and_then(Value::as_str)
        .map(normalize_board_format)?;
    let date_trigger = string_setting(&settings, DATE_TRIGGER_KEY)
        .unwrap_or_else(|| config.kanban.date_trigger.clone());
    let time_trigger = string_setting(&settings, TIME_TRIGGER_KEY)
        .unwrap_or_else(|| config.kanban.time_trigger.clone());
    Some(IndexedKanbanBoard {
        format,
        settings,
        date_trigger,
        time_trigger,
    })
}

pub fn list_kanban_boards(paths: &VaultPaths) -> Result<Vec<KanbanBoardSummary>, KanbanError> {
    let database =
        CacheDatabase::open(paths).map_err(|error| KanbanError::Message(error.to_string()))?;
    let connection = database.connection();
    let config = crate::load_vault_config(paths).config;
    let boards = load_board_rows(connection)?;

    boards
        .into_iter()
        .map(|board| {
            let columns = load_board_columns(connection, &board, &config)?;
            Ok(KanbanBoardSummary {
                title: board_title(&board.path),
                path: board.path,
                format: board.format,
                column_count: columns.len(),
                card_count: columns.iter().map(|column| column.card_count).sum(),
            })
        })
        .collect()
}

pub fn load_kanban_board(
    paths: &VaultPaths,
    board: &str,
) -> Result<KanbanBoardRecord, KanbanError> {
    let resolved = resolve_note_reference(paths, board)
        .map_err(|error| KanbanError::Message(error.to_string()))?;
    let database =
        CacheDatabase::open(paths).map_err(|error| KanbanError::Message(error.to_string()))?;
    let connection = database.connection();
    let config = crate::load_vault_config(paths).config;
    let Some(board_row) = load_board_row(connection, resolved.id.as_str())? else {
        return Err(KanbanError::Message(format!(
            "{} is not an indexed Kanban board",
            resolved.path
        )));
    };
    let columns = load_board_columns(connection, &board_row, &config)?;

    Ok(KanbanBoardRecord {
        title: board_title(&board_row.path),
        path: board_row.path,
        format: board_row.format,
        date_trigger: board_row.date_trigger,
        time_trigger: board_row.time_trigger,
        settings: board_row.settings,
        columns,
    })
}

fn load_board_rows(connection: &rusqlite::Connection) -> Result<Vec<BoardRow>, KanbanError> {
    let mut statement = connection.prepare(
        "SELECT kanban_boards.document_id,
                documents.path,
                kanban_boards.format,
                kanban_boards.date_trigger,
                kanban_boards.time_trigger,
                kanban_boards.settings_json
         FROM kanban_boards
         JOIN documents ON documents.id = kanban_boards.document_id
         ORDER BY documents.path",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(BoardRow {
            document_id: row.get(0)?,
            path: row.get(1)?,
            format: row.get(2)?,
            date_trigger: row.get(3)?,
            time_trigger: row.get(4)?,
            settings: serde_json::from_str::<Value>(&row.get::<_, String>(5)?).map_err(
                |error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                },
            )?,
        })
    })?;

    let mut boards = Vec::new();
    for row in rows {
        boards.push(row?);
    }
    Ok(boards)
}

fn load_board_row(
    connection: &rusqlite::Connection,
    document_id: &str,
) -> Result<Option<BoardRow>, KanbanError> {
    connection
        .query_row(
            "SELECT kanban_boards.document_id,
                    documents.path,
                    kanban_boards.format,
                    kanban_boards.date_trigger,
                    kanban_boards.time_trigger,
                    kanban_boards.settings_json
             FROM kanban_boards
             JOIN documents ON documents.id = kanban_boards.document_id
             WHERE kanban_boards.document_id = ?1",
            [document_id],
            |row| {
                Ok(BoardRow {
                    document_id: row.get(0)?,
                    path: row.get(1)?,
                    format: row.get(2)?,
                    date_trigger: row.get(3)?,
                    time_trigger: row.get(4)?,
                    settings: serde_json::from_str::<Value>(&row.get::<_, String>(5)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                })
            },
        )
        .optional()
        .map_err(KanbanError::from)
}

fn load_board_columns(
    connection: &rusqlite::Connection,
    board: &BoardRow,
    config: &VaultConfig,
) -> Result<Vec<KanbanColumnRecord>, KanbanError> {
    let headings = load_board_headings(connection, board.document_id.as_str())?;
    let Some(column_level) = headings.iter().map(|heading| heading.level).min() else {
        return Ok(Vec::new());
    };
    let top_level_headings = headings
        .into_iter()
        .filter(|heading| heading.level == column_level)
        .collect::<Vec<_>>();

    let mut columns = Vec::with_capacity(top_level_headings.len());
    for (index, heading) in top_level_headings.iter().enumerate() {
        let next_offset = top_level_headings
            .get(index + 1)
            .map_or(i64::MAX, |candidate| candidate.byte_offset);
        let cards = load_column_cards(connection, board, config, heading, next_offset)?;
        columns.push(KanbanColumnRecord {
            name: heading.text.clone(),
            level: heading.level,
            card_count: cards.len(),
            cards,
        });
    }
    Ok(columns)
}

fn load_board_headings(
    connection: &rusqlite::Connection,
    document_id: &str,
) -> Result<Vec<HeadingRow>, KanbanError> {
    let mut statement = connection.prepare(
        "SELECT level, text, byte_offset
         FROM headings
         WHERE document_id = ?1
         ORDER BY byte_offset",
    )?;
    let rows = statement.query_map([document_id], |row| {
        Ok(HeadingRow {
            level: u8::try_from(row.get::<_, i64>(0)?).unwrap_or_default(),
            text: row.get(1)?,
            byte_offset: row.get(2)?,
        })
    })?;

    let mut headings = Vec::new();
    for row in rows {
        headings.push(row?);
    }
    Ok(headings)
}

fn load_column_cards(
    connection: &rusqlite::Connection,
    board: &BoardRow,
    config: &VaultConfig,
    heading: &HeadingRow,
    next_offset: i64,
) -> Result<Vec<KanbanCardRecord>, KanbanError> {
    let mut statement = connection.prepare(
        "SELECT list_items.id,
                list_items.text,
                list_items.tags_json,
                list_items.outlinks_json,
                list_items.line_number,
                list_items.block_id,
                list_items.symbol,
                tasks.status_char
         FROM list_items
         LEFT JOIN tasks ON tasks.list_item_id = list_items.id
         WHERE list_items.document_id = ?1
           AND list_items.parent_item_id IS NULL
           AND list_items.byte_offset > ?2
           AND list_items.byte_offset < ?3
         ORDER BY list_items.byte_offset",
    )?;
    let rows = statement.query_map(
        rusqlite::params![board.document_id, heading.byte_offset, next_offset],
        |row| {
            let status_char = row.get::<_, Option<String>>(7)?;
            Ok(CardRow {
                id: row.get(0)?,
                text: row.get(1)?,
                tags: serde_json::from_str(&row.get::<_, String>(2)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                outlinks: serde_json::from_str(&row.get::<_, String>(3)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                line_number: row.get(4)?,
                block_id: row.get(5)?,
                symbol: row.get(6)?,
                task: status_char
                    .map(|status_char| kanban_task_status(&config.tasks.statuses, &status_char)),
            })
        },
    )?;

    let mut cards = Vec::new();
    for row in rows {
        let row = row?;
        let (date, time) = parse_card_date_time(
            row.text.as_str(),
            board.date_trigger.as_str(),
            board.time_trigger.as_str(),
        );
        cards.push(KanbanCardRecord {
            id: row.id,
            text: row.text.clone(),
            line_number: row.line_number,
            block_id: row.block_id,
            symbol: row.symbol,
            tags: row.tags,
            outlinks: row.outlinks,
            date,
            time,
            inline_fields: Value::Object(inline_fields_from_card_text(row.text.as_str(), config)?),
            task: row.task,
        });
    }
    Ok(cards)
}

fn inline_fields_from_card_text(
    text: &str,
    config: &VaultConfig,
) -> Result<Map<String, Value>, KanbanError> {
    let parsed = parse_document(text, config);
    let Some(properties) = extract_indexed_properties(&parsed, config)? else {
        return Ok(Map::new());
    };

    match serde_json::from_str::<Value>(&properties.canonical_json)? {
        Value::Object(object) => Ok(object),
        _ => Ok(Map::new()),
    }
}

fn kanban_task_status(
    config: &crate::config::TaskStatusesConfig,
    status_char: &str,
) -> KanbanTaskStatus {
    let state = config.status_state(status_char);
    KanbanTaskStatus {
        status_char: status_char.to_string(),
        status_name: state.name,
        status_type: state.status_type,
        checked: state.checked,
        completed: state.completed,
    }
}

fn board_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| path.to_string(), ToString::to_string)
}

fn merged_board_settings(
    frontmatter: Option<&serde_yaml::Value>,
    source: &str,
) -> Option<Map<String, Value>> {
    let mut settings = footer_settings(source).unwrap_or_default();
    if let Some(frontmatter_settings) = frontmatter_settings(frontmatter) {
        settings.extend(frontmatter_settings);
    }

    (!settings.is_empty()).then_some(settings)
}

fn frontmatter_settings(frontmatter: Option<&serde_yaml::Value>) -> Option<Map<String, Value>> {
    let mapping = frontmatter?.as_mapping()?;
    let mut settings = Map::new();
    for key in [
        KANBAN_FRONTMATTER_KEY,
        DATE_TRIGGER_KEY,
        TIME_TRIGGER_KEY,
        METADATA_KEYS_KEY,
        ARCHIVE_WITH_DATE_KEY,
        NEW_CARD_INSERTION_METHOD_KEY,
    ] {
        let value = mapping
            .get(serde_yaml::Value::String(key.to_string()))
            .and_then(|value| serde_json::to_value(value).ok());
        if let Some(value) = value {
            settings.insert(key.to_string(), value);
        }
    }

    (!settings.is_empty()).then_some(settings)
}

fn footer_settings(source: &str) -> Option<Map<String, Value>> {
    let lines = source.lines().collect::<Vec<_>>();
    let end = last_non_empty_line(&lines)?;
    if lines[end].trim() != "%%" {
        return None;
    }

    let start = (0..end)
        .rev()
        .find(|index| lines[*index].trim() == SETTINGS_FOOTER_MARKER)?;
    let opening_fence = ((start + 1)..end).find(|index| !lines[*index].trim().is_empty())?;
    if lines[opening_fence].trim() != "```" {
        return None;
    }

    let closing_fence = ((opening_fence + 1)..end)
        .rev()
        .find(|index| !lines[*index].trim().is_empty())?;
    if lines[closing_fence].trim() != "```" || closing_fence <= opening_fence {
        return None;
    }

    match serde_json::from_str::<Value>(&lines[(opening_fence + 1)..closing_fence].join("\n"))
        .ok()?
    {
        Value::Object(settings) => Some(settings),
        _ => None,
    }
}

fn last_non_empty_line(lines: &[&str]) -> Option<usize> {
    lines.iter().rposition(|line| !line.trim().is_empty())
}

fn string_setting(settings: &Map<String, Value>, key: &str) -> Option<String> {
    settings
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn normalize_board_format(value: &str) -> String {
    if value.eq_ignore_ascii_case("basic") {
        "board".to_string()
    } else {
        value.to_string()
    }
}

fn parse_card_date_time(
    text: &str,
    date_trigger: &str,
    time_trigger: &str,
) -> (Option<String>, Option<String>) {
    let time_match = find_trigger_value(text, time_trigger, "{", "}", &[]);
    let mut exclusions = Vec::new();
    if let Some((range, _)) = &time_match {
        exclusions.push(range.clone());
    }
    let date_match = find_trigger_value(text, date_trigger, "[[", "]]", &exclusions)
        .or_else(|| find_trigger_value(text, date_trigger, "{", "}", &exclusions));

    (
        date_match.map(|(_, value)| value),
        time_match.map(|(_, value)| value),
    )
}

fn find_trigger_value(
    text: &str,
    trigger: &str,
    open: &str,
    close: &str,
    exclusions: &[std::ops::Range<usize>],
) -> Option<(std::ops::Range<usize>, String)> {
    if trigger.is_empty() {
        return None;
    }

    let mut search_start = 0;
    while search_start < text.len() {
        let relative = text[search_start..].find(trigger)?;
        let start = search_start + relative;
        if exclusions
            .iter()
            .any(|range| start < range.end && range.start < start + trigger.len())
        {
            search_start = start + trigger.len();
            continue;
        }

        let after_trigger = start + trigger.len();
        if !text[after_trigger..].starts_with(open) {
            search_start = after_trigger;
            continue;
        }

        let content_start = after_trigger + open.len();
        let close_relative = text[content_start..].find(close)?;
        let content_end = content_start + close_relative;
        return Some((
            start..(content_end + close.len()),
            text[content_start..content_end].trim().to_string(),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::{scan_vault, ScanMode};

    use super::*;

    #[test]
    fn extracts_indexed_kanban_board_from_frontmatter_settings() {
        let source =
            "---\nkanban-plugin: board\ndate-trigger: DUE\ntime-trigger: AT\n---\n\n## Todo\n\n- Card\n";
        let parsed = parse_document(source, &VaultConfig::default());

        let board = extract_indexed_board(&parsed, source, &VaultConfig::default())
            .expect("board should parse");

        assert_eq!(board.format, "board");
        assert_eq!(board.date_trigger, "DUE");
        assert_eq!(board.time_trigger, "AT");
        assert_eq!(
            board.settings.get("kanban-plugin").and_then(Value::as_str),
            Some("board")
        );
    }

    #[test]
    fn extracts_indexed_kanban_board_from_footer_settings_comment() {
        let source = concat!(
            "## Todo\n\n",
            "- Card\n\n",
            "%% kanban:settings\n",
            "```\n",
            "{\"kanban-plugin\":\"board\",\"date-trigger\":\"DUE\",\"time-trigger\":\"AT\"}\n",
            "```\n",
            "%%\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());

        let board = extract_indexed_board(&parsed, source, &VaultConfig::default())
            .expect("board should parse");

        assert_eq!(board.format, "board");
        assert_eq!(board.date_trigger, "DUE");
        assert_eq!(board.time_trigger, "AT");
        assert_eq!(
            board.settings.get("kanban-plugin").and_then(Value::as_str),
            Some("board")
        );
    }

    #[test]
    fn load_kanban_board_groups_columns_and_extracts_card_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
        fs::write(
            vault_root.join("Projects/Alpha.md"),
            "---\nstatus: active\nowner: Ops\n---\n# Alpha\n",
        )
        .expect("linked note should exist");
        fs::write(
            vault_root.join("Board.md"),
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "date-trigger: DUE\n",
                "time-trigger: AT\n",
                "---\n\n",
                "## Todo\n\n",
                "- Release DUE{2026-04-01} AT{09:30} #ship [[Projects/Alpha]] [priority:: high]\n",
                "- [/] Waiting on review [owner:: Ops]\n\n",
                "## Done\n\n",
                "- Shipped DUE{2026-04-03}\n",
            ),
        )
        .expect("board should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let boards = list_kanban_boards(&paths).expect("boards should list");
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].path, "Board.md");
        assert_eq!(boards[0].column_count, 2);
        assert_eq!(boards[0].card_count, 3);

        let board = load_kanban_board(&paths, "Board").expect("board should load");
        assert_eq!(board.path, "Board.md");
        assert_eq!(board.date_trigger, "DUE");
        assert_eq!(board.time_trigger, "AT");
        assert_eq!(
            board
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Todo", "Done"]
        );

        let first_card = &board.columns[0].cards[0];
        assert_eq!(first_card.date.as_deref(), Some("2026-04-01"));
        assert_eq!(first_card.time.as_deref(), Some("09:30"));
        assert_eq!(first_card.tags, vec!["#ship".to_string()]);
        assert_eq!(first_card.outlinks, vec!["[[Projects/Alpha]]".to_string()]);
        assert_eq!(
            first_card
                .inline_fields
                .get("priority")
                .and_then(Value::as_str),
            Some("high")
        );

        let second_card = &board.columns[0].cards[1];
        assert_eq!(
            second_card.task,
            Some(KanbanTaskStatus {
                status_char: "/".to_string(),
                status_name: "In Progress".to_string(),
                status_type: "IN_PROGRESS".to_string(),
                checked: true,
                completed: false,
            })
        );
        assert_eq!(
            second_card
                .inline_fields
                .get("owner")
                .and_then(Value::as_str),
            Some("Ops")
        );
    }

    #[test]
    fn scan_indexes_footer_only_kanban_boards() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Board.md"),
            concat!(
                "## Todo\n\n",
                "- Card\n\n",
                "%% kanban:settings\n",
                "```\n",
                "{\"kanban-plugin\":\"board\",\"date-trigger\":\"DUE\",\"time-trigger\":\"AT\"}\n",
                "```\n",
                "%%\n",
            ),
        )
        .expect("board should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let boards = list_kanban_boards(&paths).expect("boards should list");
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].path, "Board.md");

        let board = load_kanban_board(&paths, "Board").expect("board should load");
        assert_eq!(board.date_trigger, "DUE");
        assert_eq!(board.time_trigger, "AT");
        assert_eq!(board.columns.len(), 1);
    }
}
