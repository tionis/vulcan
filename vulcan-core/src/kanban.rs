use std::fmt::{Display, Formatter, Write as _};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::cache::CacheDatabase;
use crate::expression::functions::format_date;
use crate::extract_indexed_properties;
use crate::parse_document;
use crate::paths::VaultPaths;
use crate::resolve_note_reference;
use crate::{scan_vault, ParsedDocument, ScanMode, VaultConfig};

const KANBAN_FRONTMATTER_KEY: &str = "kanban-plugin";
const DATE_TRIGGER_KEY: &str = "date-trigger";
const TIME_TRIGGER_KEY: &str = "time-trigger";
const METADATA_KEYS_KEY: &str = "metadata-keys";
const ARCHIVE_WITH_DATE_KEY: &str = "archive-with-date";
const APPEND_ARCHIVE_DATE_KEY: &str = "append-archive-date";
const ARCHIVE_DATE_FORMAT_KEY: &str = "archive-date-format";
const NEW_CARD_INSERTION_METHOD_KEY: &str = "new-card-insertion-method";
const SETTINGS_FOOTER_MARKER: &str = "%% kanban:settings";
const DEFAULT_ARCHIVE_COLUMN_NAME: &str = "Archive";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct KanbanArchiveReport {
    pub path: String,
    pub title: String,
    pub source_column: String,
    pub archive_column: String,
    pub card_id: String,
    pub card_text: String,
    pub archived_text: String,
    pub line_number: i64,
    pub dry_run: bool,
    pub created_archive_column: bool,
    pub archive_with_date_applied: bool,
    pub rescanned: bool,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnLayout {
    name: String,
    max_items: Option<usize>,
    completes_cards: bool,
    archived: bool,
}

#[derive(Debug, Clone)]
struct BoardColumnState {
    heading: HeadingRow,
    next_offset: i64,
    layout: ColumnLayout,
    cards: Vec<KanbanCardRecord>,
}

#[derive(Debug, Clone)]
struct ArchiveBehavior {
    with_date: bool,
    append_after_title: bool,
    date_format: String,
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
            let columns = load_board_columns(paths, connection, &board, &config, false)?;
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
    include_archive: bool,
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
    let columns = load_board_columns(paths, connection, &board_row, &config, include_archive)?;

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

#[allow(clippy::too_many_lines)]
pub fn archive_kanban_card(
    paths: &VaultPaths,
    board: &str,
    card: &str,
    dry_run: bool,
) -> Result<KanbanArchiveReport, KanbanError> {
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

    let source_path = paths.vault_root().join(&board_row.path);
    let source = fs::read_to_string(&source_path).map_err(|error| {
        KanbanError::Message(format!("failed to read {}: {error}", board_row.path))
    })?;
    let column_states = load_board_column_states(paths, connection, &board_row, &config, true)?;
    let (source_column_index, card_index) = resolve_card_match(&column_states, card)?;
    let source_column = &column_states[source_column_index];
    if source_column.layout.archived {
        return Err(KanbanError::Message(format!(
            "card {} is already archived in {}",
            card, source_column.layout.name
        )));
    }
    let card_record = &source_column.cards[card_index];

    let parsed = parse_document(&source, &config);
    let card_line_number = usize::try_from(card_record.line_number).ok();
    let Some((raw_item_index, _raw_item)) = parsed.list_items.iter().enumerate().find(|item| {
        item.1.parent_item_index.is_none() && Some(item.1.line_number) == card_line_number
    }) else {
        return Err(KanbanError::Message(format!(
            "failed to locate card at line {} in {}",
            card_record.line_number, board_row.path
        )));
    };

    let line_starts = line_start_offsets(&source);
    let footer_start = footer_settings_start_offset(&source, &line_starts);
    let source_column_end = usize::try_from(source_column.next_offset)
        .ok()
        .map_or(source.len(), |offset| offset.min(source.len()));
    let card_range = byte_range_for_list_item_subtree(
        &parsed.list_items,
        raw_item_index,
        source_column_end.min(footer_start.unwrap_or(source.len())),
        source.len(),
    )?;
    let behavior = archive_behavior(board_row.settings.as_object(), &config);
    let archived_block = archive_card_block(&source[card_range.clone()], &behavior);
    let archived_text = card_record_archived_text(card_record, &archived_block);

    let updated = if let Some(index) = column_states.iter().position(|state| state.layout.archived)
    {
        let state = &column_states[index];
        let archive_insertion = archive_insertion_offset(state, footer_start, &source);
        apply_text_edits(
            &source,
            &[
                (archive_insertion..archive_insertion, archived_block.clone()),
                (card_range.clone(), String::new()),
            ],
        )
    } else {
        let archive_start = footer_start.unwrap_or(source.len());
        let archive_section = build_archive_section(
            archived_block.as_str(),
            source_column.heading.level,
            &source[..archive_start],
        );
        apply_text_edits(
            &source,
            &[
                (archive_start..archive_start, archive_section),
                (card_range.clone(), String::new()),
            ],
        )
    };

    if !dry_run {
        fs::write(&source_path, updated).map_err(|error| {
            KanbanError::Message(format!("failed to write {}: {error}", board_row.path))
        })?;
        scan_vault(paths, ScanMode::Incremental)
            .map_err(|error| KanbanError::Message(error.to_string()))?;
    }

    Ok(KanbanArchiveReport {
        path: board_row.path.clone(),
        title: board_title(&board_row.path),
        source_column: source_column.layout.name.clone(),
        archive_column: column_states
            .iter()
            .find(|state| state.layout.archived)
            .map_or_else(
                || DEFAULT_ARCHIVE_COLUMN_NAME.to_string(),
                |state| state.layout.name.clone(),
            ),
        card_id: card_record.id.clone(),
        card_text: card_record.text.clone(),
        archived_text,
        line_number: card_record.line_number,
        dry_run,
        created_archive_column: !column_states.iter().any(|state| state.layout.archived),
        archive_with_date_applied: behavior.with_date,
        rescanned: !dry_run,
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
    paths: &VaultPaths,
    connection: &rusqlite::Connection,
    board: &BoardRow,
    config: &VaultConfig,
    include_archive: bool,
) -> Result<Vec<KanbanColumnRecord>, KanbanError> {
    let states = load_board_column_states(paths, connection, board, config, include_archive)?;
    Ok(states
        .into_iter()
        .map(|state| KanbanColumnRecord {
            name: state.layout.name,
            level: state.heading.level,
            card_count: state.cards.len(),
            cards: state.cards,
        })
        .collect())
}

fn load_board_column_states(
    paths: &VaultPaths,
    connection: &rusqlite::Connection,
    board: &BoardRow,
    config: &VaultConfig,
    include_archive: bool,
) -> Result<Vec<BoardColumnState>, KanbanError> {
    let headings = load_board_headings(connection, board.document_id.as_str())?;
    let Some(column_level) = headings.iter().map(|heading| heading.level).min() else {
        return Ok(Vec::new());
    };
    let top_level_headings = headings
        .into_iter()
        .filter(|heading| heading.level == column_level)
        .collect::<Vec<_>>();
    let layouts = load_column_layouts(paths, &board.path, &top_level_headings);

    let mut columns = Vec::with_capacity(top_level_headings.len());
    for (index, heading) in top_level_headings.iter().enumerate() {
        let next_offset = top_level_headings
            .get(index + 1)
            .map_or(i64::MAX, |candidate| candidate.byte_offset);
        let cards = load_column_cards(connection, board, config, heading, next_offset)?;
        let layout = layouts
            .get(index)
            .cloned()
            .unwrap_or_else(|| default_column_layout(heading.text.as_str()));
        if layout.archived && !include_archive {
            continue;
        }
        columns.push(BoardColumnState {
            heading: heading.clone(),
            next_offset,
            layout,
            cards,
        });
    }
    Ok(columns)
}

fn resolve_card_match(
    columns: &[BoardColumnState],
    identifier: &str,
) -> Result<(usize, usize), KanbanError> {
    let mut exact_matches = Vec::new();
    let mut text_matches = Vec::new();

    for (column_index, column) in columns.iter().enumerate() {
        for (card_index, card) in column.cards.iter().enumerate() {
            let line_match = card.line_number.to_string() == identifier;
            let id_match = card.id == identifier;
            let block_match = card.block_id.as_deref() == Some(identifier);
            if id_match || block_match || line_match {
                exact_matches.push((column_index, card_index));
                continue;
            }
            if card.text == identifier || card.text.eq_ignore_ascii_case(identifier) {
                text_matches.push((column_index, card_index));
            }
        }
    }

    let matches = if exact_matches.is_empty() {
        text_matches
    } else {
        exact_matches
    };

    match matches.as_slice() {
        [(column_index, card_index)] => Ok((*column_index, *card_index)),
        [] => Err(KanbanError::Message(format!(
            "no Kanban card matched {identifier}"
        ))),
        _ => {
            let matches = matches
                .into_iter()
                .map(|(column_index, card_index)| {
                    let column = &columns[column_index];
                    let card = &column.cards[card_index];
                    format!("{}:{}: {}", column.layout.name, card.line_number, card.text)
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(KanbanError::Message(format!(
                "card {identifier} matched multiple entries: {matches}"
            )))
        }
    }
}

fn archive_behavior(
    settings: Option<&Map<String, Value>>,
    config: &VaultConfig,
) -> ArchiveBehavior {
    ArchiveBehavior {
        with_date: settings
            .and_then(|settings| bool_setting(settings, ARCHIVE_WITH_DATE_KEY))
            .unwrap_or(config.kanban.archive_with_date),
        append_after_title: settings
            .and_then(|settings| bool_setting(settings, APPEND_ARCHIVE_DATE_KEY))
            .unwrap_or(config.kanban.append_archive_date),
        date_format: settings
            .and_then(|settings| string_setting(settings, ARCHIVE_DATE_FORMAT_KEY))
            .unwrap_or_else(|| config.kanban.archive_date_format.clone()),
    }
}

fn archive_card_block(block_text: &str, behavior: &ArchiveBehavior) -> String {
    if !behavior.with_date {
        return block_text.to_string();
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX);
    let timestamp = format_date(now_ms, behavior.date_format.as_str());

    if let Some(line_break) = block_text.find('\n') {
        format!(
            "{}{}",
            archive_card_line(
                &block_text[..line_break],
                timestamp.as_str(),
                behavior.append_after_title
            ),
            &block_text[line_break..]
        )
    } else {
        archive_card_line(block_text, timestamp.as_str(), behavior.append_after_title)
    }
}

fn archive_card_line(line: &str, timestamp: &str, append_after_title: bool) -> String {
    let Some((prefix, content)) = split_list_item_prefix(line) else {
        return line.to_string();
    };
    let (content, block_suffix) = split_block_id_suffix(content);
    let content = content.trim_end();

    let updated_content = if content.is_empty() {
        timestamp.to_string()
    } else if append_after_title {
        format!("{content} {timestamp}")
    } else {
        format!("{timestamp} {content}")
    };

    format!("{prefix}{updated_content}{block_suffix}")
}

fn split_list_item_prefix(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let indent_len = line.len().saturating_sub(trimmed.len());

    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        if let Some(task_end) = rest.strip_prefix('[').and_then(|rest| rest.find("] ")) {
            let prefix_end = indent_len + 2 + task_end + 3;
            return Some((&line[..prefix_end], &line[prefix_end..]));
        }
        let prefix_end = indent_len + 2;
        return Some((&line[..prefix_end], &line[prefix_end..]));
    }

    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        let prefix_end = indent_len + digits + 2;
        return Some((&line[..prefix_end], &line[prefix_end..]));
    }

    None
}

fn split_block_id_suffix(content: &str) -> (&str, &str) {
    let Some(index) = content.rfind(" ^") else {
        return (content, "");
    };
    let suffix = &content[(index + 2)..];
    if suffix
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        (&content[..index], &content[index..])
    } else {
        (content, "")
    }
}

fn archive_insertion_offset(
    column: &BoardColumnState,
    footer_start: Option<usize>,
    source: &str,
) -> usize {
    let next_offset = usize::try_from(column.next_offset)
        .ok()
        .map_or(source.len(), |offset| offset.min(source.len()));
    footer_start.map_or(next_offset, |footer_start| next_offset.min(footer_start))
}

fn build_archive_section(card_block: &str, heading_level: u8, prefix: &str) -> String {
    let mut section = String::new();
    if !prefix.is_empty() && !prefix.ends_with('\n') {
        section.push('\n');
    }
    if !prefix.is_empty() && !prefix.ends_with("\n\n") {
        section.push('\n');
    }
    section.push_str("***\n\n");
    let _ = writeln!(
        section,
        "{} {}",
        "#".repeat(usize::from(heading_level)),
        DEFAULT_ARCHIVE_COLUMN_NAME
    );
    section.push('\n');
    section.push_str(card_block.trim_end_matches('\n'));
    section.push('\n');
    section.push('\n');
    section
}

fn card_record_archived_text(card: &KanbanCardRecord, archived_block: &str) -> String {
    let line = archived_block
        .lines()
        .next()
        .unwrap_or(card.text.as_str())
        .trim();
    if let Some((_, content)) = split_list_item_prefix(line) {
        split_block_id_suffix(content).0.trim().to_string()
    } else {
        line.to_string()
    }
}

fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in source.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn byte_range_for_list_item_subtree(
    list_items: &[crate::RawListItem],
    item_index: usize,
    section_end: usize,
    source_len: usize,
) -> Result<std::ops::Range<usize>, KanbanError> {
    let Some(item) = list_items.get(item_index) else {
        return Err(KanbanError::Message(format!(
            "failed to resolve list item {item_index} for card mutation"
        )));
    };

    let start = item.byte_offset.min(source_len);
    let mut end = section_end.min(source_len);
    for (candidate_index, candidate) in list_items.iter().enumerate().skip(item_index + 1) {
        if !is_descendant_item(list_items, item_index, candidate_index) {
            end = candidate.byte_offset.min(end);
            break;
        }
    }
    Ok(start..end)
}

fn is_descendant_item(
    list_items: &[crate::RawListItem],
    ancestor_index: usize,
    candidate_index: usize,
) -> bool {
    let mut current = list_items
        .get(candidate_index)
        .and_then(|item| item.parent_item_index);
    while let Some(parent_index) = current {
        if parent_index == ancestor_index {
            return true;
        }
        current = list_items
            .get(parent_index)
            .and_then(|item| item.parent_item_index);
    }
    false
}

fn footer_settings_start_offset(source: &str, line_starts: &[usize]) -> Option<usize> {
    let lines = source.lines().collect::<Vec<_>>();
    let end = last_non_empty_line(&lines)?;
    if lines[end].trim() != "%%" {
        return None;
    }

    let start = (0..end)
        .rev()
        .find(|index| lines[*index].trim() == SETTINGS_FOOTER_MARKER)?;
    line_starts.get(start).copied()
}

fn apply_text_edits(source: &str, edits: &[(std::ops::Range<usize>, String)]) -> String {
    let mut updated = source.to_string();
    let mut ordered = edits.to_vec();
    ordered.sort_by(|left, right| right.0.start.cmp(&left.0.start));
    for (range, replacement) in ordered {
        updated.replace_range(range, replacement.as_str());
    }
    updated
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

fn load_column_layouts(
    paths: &VaultPaths,
    board_path: &str,
    headings: &[HeadingRow],
) -> Vec<ColumnLayout> {
    let Ok(source) = fs::read_to_string(paths.vault_root().join(board_path)) else {
        return headings
            .iter()
            .map(|heading| default_column_layout(heading.text.as_str()))
            .collect();
    };

    headings
        .iter()
        .enumerate()
        .map(|(index, heading)| {
            let next_offset = headings.get(index + 1).map_or(source.len(), |candidate| {
                clamp_offset(candidate.byte_offset, &source)
            });
            column_layout_from_section(&source, heading, next_offset)
        })
        .collect()
}

fn column_layout_from_section(
    source: &str,
    heading: &HeadingRow,
    next_offset: usize,
) -> ColumnLayout {
    let mut layout = default_column_layout(heading.text.as_str());
    let heading_offset = clamp_offset(heading.byte_offset, source);
    layout.archived =
        previous_non_empty_line(source, heading_offset).is_some_and(|line| line == "***");
    layout.completes_cards = section_has_complete_marker(source, heading_offset, next_offset);
    layout
}

fn default_column_layout(text: &str) -> ColumnLayout {
    let (name, max_items) = parse_column_heading(text);
    ColumnLayout {
        name,
        max_items,
        completes_cards: false,
        archived: false,
    }
}

fn parse_column_heading(text: &str) -> (String, Option<usize>) {
    let Some((prefix, suffix)) = text.rsplit_once('(') else {
        return (text.to_string(), None);
    };
    let count = suffix.trim_end();
    let Some(count) = count.strip_suffix(')') else {
        return (text.to_string(), None);
    };
    let Ok(max_items) = count.trim().parse::<usize>() else {
        return (text.to_string(), None);
    };

    (prefix.trim_end().to_string(), Some(max_items))
}

fn clamp_offset(offset: i64, source: &str) -> usize {
    usize::try_from(offset)
        .ok()
        .map_or(source.len(), |offset| offset.min(source.len()))
}

fn previous_non_empty_line(source: &str, offset: usize) -> Option<&str> {
    source[..offset]
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

fn section_has_complete_marker(source: &str, heading_offset: usize, next_offset: usize) -> bool {
    let section_start = source[heading_offset..next_offset]
        .find('\n')
        .map_or(next_offset, |relative| heading_offset + relative + 1);
    let mut marker_found = false;

    for line in source[section_start..next_offset].lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_list_item_line(trimmed) {
            return marker_found;
        }
        if is_complete_marker_line(trimmed) {
            marker_found = true;
        }
    }

    marker_found
}

fn is_list_item_line(line: &str) -> bool {
    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || is_numbered_list_item(line)
}

fn is_numbered_list_item(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". ")
}

fn is_complete_marker_line(line: &str) -> bool {
    normalize_wrapped_marker(line) == "Complete"
}

fn normalize_wrapped_marker(line: &str) -> String {
    line.trim_matches(|ch: char| ch == '*' || ch == '_' || ch.is_whitespace())
        .to_string()
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
        APPEND_ARCHIVE_DATE_KEY,
        ARCHIVE_DATE_FORMAT_KEY,
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

fn bool_setting(settings: &Map<String, Value>, key: &str) -> Option<bool> {
    settings.get(key).and_then(Value::as_bool)
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

        let board = load_kanban_board(&paths, "Board", false).expect("board should load");
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

        let board = load_kanban_board(&paths, "Board", false).expect("board should load");
        assert_eq!(board.date_trigger, "DUE");
        assert_eq!(board.time_trigger, "AT");
        assert_eq!(board.columns.len(), 1);
    }

    #[test]
    fn parses_column_layouts_from_markdown_sections() {
        let source = concat!(
            "## Todo (2)\n\n",
            "- Build release\n\n",
            "## Done\n\n",
            "**Complete**\n\n",
            "- Shipped\n\n",
            "***\n\n",
            "## Archive\n\n",
            "- Old card\n",
        );
        let parsed = parse_document(source, &VaultConfig::default());
        let headings = parsed
            .headings
            .iter()
            .map(|heading| HeadingRow {
                level: heading.level,
                text: heading.text.clone(),
                byte_offset: i64::try_from(heading.byte_offset).expect("offset should fit in i64"),
            })
            .collect::<Vec<_>>();

        let layouts = headings
            .iter()
            .enumerate()
            .map(|(index, heading)| {
                let next_offset = headings.get(index + 1).map_or(source.len(), |candidate| {
                    clamp_offset(candidate.byte_offset, source)
                });
                column_layout_from_section(source, heading, next_offset)
            })
            .collect::<Vec<_>>();

        assert_eq!(
            layouts,
            vec![
                ColumnLayout {
                    name: "Todo".to_string(),
                    max_items: Some(2),
                    completes_cards: false,
                    archived: false,
                },
                ColumnLayout {
                    name: "Done".to_string(),
                    max_items: None,
                    completes_cards: true,
                    archived: false,
                },
                ColumnLayout {
                    name: "Archive".to_string(),
                    max_items: None,
                    completes_cards: false,
                    archived: true,
                },
            ]
        );
    }

    #[test]
    fn load_kanban_board_excludes_archive_sections_from_default_columns() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Board.md"),
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "---\n\n",
                "## Todo (2)\n\n",
                "- Build release\n",
                "- [/] Waiting on review\n\n",
                "## Done\n\n",
                "**Complete**\n\n",
                "- [x] Shipped\n\n",
                "***\n\n",
                "## Archive\n\n",
                "- Old card\n",
            ),
        )
        .expect("board should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let boards = list_kanban_boards(&paths).expect("boards should list");
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].column_count, 2);
        assert_eq!(boards[0].card_count, 3);

        let board = load_kanban_board(&paths, "Board", false).expect("board should load");
        assert_eq!(
            board
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Todo", "Done"]
        );
        assert_eq!(board.columns[0].card_count, 2);
        assert_eq!(board.columns[1].card_count, 1);

        let with_archive =
            load_kanban_board(&paths, "Board", true).expect("board should load with archive");
        assert_eq!(
            with_archive
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Todo", "Done", "Archive"]
        );
        assert_eq!(with_archive.columns[2].card_count, 1);
    }

    #[test]
    fn archive_card_line_keeps_block_ids_when_rewriting_titles() {
        assert_eq!(
            archive_card_line("- Ship release ^ship-release", "2026-03-29 09:00", false),
            "- 2026-03-29 09:00 Ship release ^ship-release"
        );
        assert_eq!(
            archive_card_line("- [/] Ship release ^ship-release", "2026-03-29 09:00", true),
            "- [/] Ship release 2026-03-29 09:00 ^ship-release"
        );
    }

    #[test]
    fn archive_kanban_card_moves_cards_into_existing_archive_section() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Board.md"),
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "---\n\n",
                "## Todo\n\n",
                "- Build release ^build-release\n",
                "  - Confirm notes\n\n",
                "## Done\n\n",
                "- Shipped\n\n",
                "***\n\n",
                "## Archive\n\n",
                "- Old card\n",
            ),
        )
        .expect("board should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report =
            archive_kanban_card(&paths, "Board", "build-release", false).expect("archive works");
        assert_eq!(report.source_column, "Todo");
        assert_eq!(report.archive_column, "Archive");
        assert_eq!(report.card_text, "Build release ^build-release");
        assert_eq!(report.archived_text, "Build release");
        assert!(!report.created_archive_column);
        assert!(!report.archive_with_date_applied);
        assert!(report.rescanned);

        let source =
            fs::read_to_string(vault_root.join("Board.md")).expect("board should remain readable");
        assert!(!source.contains("- Build release ^build-release\n  - Confirm notes\n\n## Done"));
        assert!(source.contains("## Archive\n\n- Old card"));

        let board = load_kanban_board(&paths, "Board", true).expect("board should load");
        assert_eq!(board.columns[0].card_count, 0);
        assert_eq!(board.columns[2].card_count, 2);
        assert!(board.columns[2].cards[1].text.starts_with("Build release"));
        assert_eq!(
            board.columns[2].cards[1].block_id.as_deref(),
            Some("build-release")
        );
    }

    #[test]
    fn archive_kanban_card_creates_archive_section_before_footer_settings() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Board.md"),
            concat!(
                "---\n",
                "kanban-plugin: board\n",
                "---\n\n",
                "## Todo\n\n",
                "- Build release\n\n",
                "## Done\n\n",
                "- Shipped\n\n",
                "%% kanban:settings\n",
                "```\n",
                "{\"kanban-plugin\":\"board\"}\n",
                "```\n",
                "%%\n",
            ),
        )
        .expect("board should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let report =
            archive_kanban_card(&paths, "Board", "Build release", false).expect("archive works");
        assert!(report.created_archive_column);
        assert_eq!(report.archive_column, "Archive");

        let source =
            fs::read_to_string(vault_root.join("Board.md")).expect("board should remain readable");
        assert!(source.contains("***\n\n## Archive\n\n- Build release\n\n%% kanban:settings"));

        let board = load_kanban_board(&paths, "Board", true).expect("board should load");
        assert_eq!(
            board
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Todo", "Done", "Archive"]
        );
        assert_eq!(board.columns[2].card_count, 1);
        assert_eq!(board.columns[2].cards[0].text, "Build release");
    }
}
