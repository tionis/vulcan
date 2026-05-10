#![allow(clippy::too_many_lines)]

use crate::commit::AutoCommitPolicy;
use crate::output::{
    paginated_items, print_json, print_json_lines, print_selected_human_fields, ListOutputControls,
};
use crate::{warn_auto_commit_if_needed, AnsiPalette, Cli, CliError, KanbanCommand, OutputFormat};
use serde::Serialize;
use serde_json::Value;
use vulcan_core::{
    add_kanban_card, archive_kanban_card, list_kanban_boards, load_kanban_board, move_kanban_card,
    KanbanAddReport, KanbanArchiveReport, KanbanBoardRecord, KanbanBoardSummary, KanbanMoveReport,
    KanbanTaskStatus, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
struct KanbanCardsReport {
    board_path: String,
    board_title: String,
    column_filter: Option<String>,
    status_filter: Option<String>,
    result_count: usize,
    cards: Vec<KanbanCardListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct KanbanCardListItem {
    board_path: String,
    board_title: String,
    column: String,
    id: String,
    text: String,
    line_number: i64,
    block_id: Option<String>,
    symbol: String,
    tags: Vec<String>,
    outlinks: Vec<String>,
    date: Option<String>,
    time: Option<String>,
    inline_fields: Value,
    metadata: serde_json::Value,
    task: Option<KanbanTaskStatus>,
}

pub(crate) fn handle_kanban_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &KanbanCommand,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        KanbanCommand::List => {
            let boards = list_kanban_boards(paths).map_err(CliError::operation)?;
            print_kanban_board_list(
                cli.output,
                &boards,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        KanbanCommand::Show {
            board,
            verbose,
            include_archive,
        } => {
            let report =
                load_kanban_board(paths, board, *include_archive).map_err(CliError::operation)?;
            print_kanban_board_report(cli.output, &report, *verbose)
        }
        KanbanCommand::Cards {
            board,
            column,
            status,
        } => {
            let report =
                run_kanban_cards_command(paths, board, column.as_deref(), status.as_deref())?;
            print_kanban_cards_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        KanbanCommand::Archive {
            board,
            card,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_kanban_archive_command(paths, board, card, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-archive",
                        &kanban_archive_changed_files(&report),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_kanban_archive_report(cli.output, &report)
        }
        KanbanCommand::Move {
            board,
            card,
            target_column,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_kanban_move_command(paths, board, card, target_column, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-move",
                        &kanban_move_changed_files(&report),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_kanban_move_report(cli.output, &report)
        }
        KanbanCommand::Add {
            board,
            column,
            text,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = run_kanban_add_command(paths, board, column, text, *dry_run)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "kanban-add",
                        &kanban_add_changed_files(&report),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_kanban_add_report(cli.output, &report)
        }
    }
}

fn run_kanban_cards_command(
    paths: &VaultPaths,
    board: &str,
    column: Option<&str>,
    status: Option<&str>,
) -> Result<KanbanCardsReport, CliError> {
    let board = load_kanban_board(paths, board, false).map_err(CliError::operation)?;
    let column_filter = normalize_optional_filter(column);
    let status_filter = normalize_optional_filter(status);
    let mut cards = Vec::new();

    for column_record in &board.columns {
        if !kanban_column_matches(column_record.name.as_str(), column_filter.as_deref()) {
            continue;
        }

        for card in &column_record.cards {
            if !kanban_status_matches(card.task.as_ref(), status_filter.as_deref()) {
                continue;
            }

            cards.push(KanbanCardListItem {
                board_path: board.path.clone(),
                board_title: board.title.clone(),
                column: column_record.name.clone(),
                id: card.id.clone(),
                text: card.text.clone(),
                line_number: card.line_number,
                block_id: card.block_id.clone(),
                symbol: card.symbol.clone(),
                tags: card.tags.clone(),
                outlinks: card.outlinks.clone(),
                date: card.date.clone(),
                time: card.time.clone(),
                inline_fields: card.inline_fields.clone(),
                metadata: card.metadata.clone(),
                task: card.task.clone(),
            });
        }
    }

    Ok(KanbanCardsReport {
        board_path: board.path,
        board_title: board.title,
        column_filter,
        status_filter,
        result_count: cards.len(),
        cards,
    })
}

fn run_kanban_archive_command(
    paths: &VaultPaths,
    board: &str,
    card: &str,
    dry_run: bool,
) -> Result<KanbanArchiveReport, CliError> {
    archive_kanban_card(paths, board, card, dry_run).map_err(CliError::operation)
}

fn run_kanban_move_command(
    paths: &VaultPaths,
    board: &str,
    card: &str,
    target_column: &str,
    dry_run: bool,
) -> Result<KanbanMoveReport, CliError> {
    move_kanban_card(paths, board, card, target_column, dry_run).map_err(CliError::operation)
}

fn run_kanban_add_command(
    paths: &VaultPaths,
    board: &str,
    column: &str,
    text: &str,
    dry_run: bool,
) -> Result<KanbanAddReport, CliError> {
    add_kanban_card(paths, board, column, text, dry_run).map_err(CliError::operation)
}

fn normalize_optional_filter(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn kanban_column_matches(name: &str, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };

    name == filter || name.eq_ignore_ascii_case(filter)
}

fn kanban_status_matches(task: Option<&KanbanTaskStatus>, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(task) = task else {
        return false;
    };

    task.status_char == filter
        || task.status_char.eq_ignore_ascii_case(filter)
        || task.status_name.eq_ignore_ascii_case(filter)
        || task.status_type.eq_ignore_ascii_case(filter)
}

fn kanban_archive_changed_files(report: &KanbanArchiveReport) -> Vec<String> {
    vec![report.path.clone()]
}

fn kanban_move_changed_files(report: &KanbanMoveReport) -> Vec<String> {
    vec![report.path.clone()]
}

fn kanban_add_changed_files(report: &KanbanAddReport) -> Vec<String> {
    vec![report.path.clone()]
}

fn print_kanban_board_list(
    output: OutputFormat,
    boards: &[KanbanBoardSummary],
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_boards = paginated_items(boards, list_controls);
    let rows = kanban_board_rows(visible_boards);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Kanban boards"));
            }
            if visible_boards.is_empty() {
                println!("No indexed Kanban boards.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for board in visible_boards {
                    println!(
                        "- {} ({}) [{}] {} column(s), {} card(s)",
                        board.title, board.path, board.format, board.column_count, board.card_count
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_kanban_board_report(
    output: OutputFormat,
    report: &KanbanBoardRecord,
    verbose: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let card_count = report
                .columns
                .iter()
                .map(|column| column.card_count)
                .sum::<usize>();
            println!("{} ({})", report.title, report.path);
            println!("Format: {}", report.format);
            println!("Columns: {}", report.columns.len());
            println!("Cards: {card_count}");
            println!("Date trigger: {}", report.date_trigger);
            println!("Time trigger: {}", report.time_trigger);
            if report.columns.is_empty() {
                println!("No columns.");
                return Ok(());
            }

            for column in &report.columns {
                println!();
                println!("{} ({})", column.name, column.card_count);
                if !verbose {
                    continue;
                }
                for card in &column.cards {
                    print_kanban_card_summary(card);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_cards_report(
    output: OutputFormat,
    report: &KanbanCardsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    let visible_cards = paginated_items(&report.cards, list_controls);
    let rows = kanban_card_rows(report, visible_cards);
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Kanban cards for"),
                    palette.bold(&report.board_title)
                );
            }
            if visible_cards.is_empty() {
                println!("No matching Kanban cards.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }

            let mut current_column: Option<&str> = None;
            for card in visible_cards {
                if current_column != Some(card.column.as_str()) {
                    current_column = Some(card.column.as_str());
                    println!("{}", card.column);
                }
                print_kanban_card_list_item(card);
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_kanban_archive_report(
    output: OutputFormat,
    report: &KanbanArchiveReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!(
                    "Dry run: archive {} from {} to {} in {}",
                    report.card_id, report.source_column, report.archive_column, report.path
                );
            } else {
                println!(
                    "Archived {} from {} to {} in {}",
                    report.card_id, report.source_column, report.archive_column, report.path
                );
            }
            println!("Card: {}", report.card_text);
            if report.created_archive_column {
                println!("Created archive column: {}", report.archive_column);
            }
            if report.archive_with_date_applied {
                println!("Archived text: {}", report.archived_text);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_move_report(
    output: OutputFormat,
    report: &KanbanMoveReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!(
                    "Dry run: move {} from {} to {} in {}",
                    report.card_id, report.source_column, report.target_column, report.path
                );
            } else {
                println!(
                    "Moved {} from {} to {} in {}",
                    report.card_id, report.source_column, report.target_column, report.path
                );
            }
            println!("Card: {}", report.card_text);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_kanban_add_report(output: OutputFormat, report: &KanbanAddReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: add card to {} in {}", report.column, report.path);
            } else {
                println!("Added card to {} in {}", report.column, report.path);
            }
            println!("Card: {}", report.card_text);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn kanban_board_rows(boards: &[KanbanBoardSummary]) -> Vec<Value> {
    boards
        .iter()
        .map(|board| {
            serde_json::json!({
                "path": board.path,
                "title": board.title,
                "format": board.format,
                "column_count": board.column_count,
                "card_count": board.card_count,
            })
        })
        .collect()
}

fn kanban_card_rows(report: &KanbanCardsReport, cards: &[KanbanCardListItem]) -> Vec<Value> {
    cards
        .iter()
        .map(|card| {
            serde_json::json!({
                "board_path": report.board_path,
                "board_title": report.board_title,
                "column_filter": report.column_filter,
                "status_filter": report.status_filter,
                "column": card.column,
                "card_id": card.id,
                "text": card.text,
                "line_number": card.line_number,
                "block_id": card.block_id,
                "symbol": card.symbol,
                "tags": card.tags,
                "outlinks": card.outlinks,
                "date": card.date,
                "time": card.time,
                "inline_fields": card.inline_fields,
                "metadata": card.metadata,
                "task": card.task,
                "task_status_char": card.task.as_ref().map(|task| task.status_char.clone()),
                "task_status_name": card.task.as_ref().map(|task| task.status_name.clone()),
                "task_status_type": card.task.as_ref().map(|task| task.status_type.clone()),
                "task_checked": card.task.as_ref().map(|task| task.checked),
                "task_completed": card.task.as_ref().map(|task| task.completed),
            })
        })
        .collect()
}

fn print_kanban_card_summary(card: &vulcan_core::KanbanCardRecord) {
    let mut details = vec![format!("line {}", card.line_number)];
    if let Some(date) = card.date.as_deref() {
        details.push(format!("date {date}"));
    }
    if let Some(time) = card.time.as_deref() {
        details.push(format!("time {time}"));
    }
    if !card.tags.is_empty() {
        details.push(format!("tags {}", card.tags.join(", ")));
    }
    if !card.outlinks.is_empty() {
        details.push(format!("links {}", card.outlinks.join(", ")));
    }
    if let Some(task) = card.task.as_ref() {
        println!(
            "- [{}] {} ({})",
            task.status_char,
            card.text,
            details.join(", ")
        );
    } else {
        println!("- {} ({})", card.text, details.join(", "));
    }
}

fn print_kanban_card_list_item(card: &KanbanCardListItem) {
    let mut details = vec![format!("line {}", card.line_number)];
    if let Some(date) = card.date.as_deref() {
        details.push(format!("date {date}"));
    }
    if let Some(time) = card.time.as_deref() {
        details.push(format!("time {time}"));
    }
    if let Some(task) = card.task.as_ref() {
        println!(
            "- [{}] {} ({})",
            task.status_char,
            card.text,
            details.join(", ")
        );
    } else {
        println!("- {} ({})", card.text, details.join(", "));
    }
}
