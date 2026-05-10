use crate::output::{
    paginated_items, print_json, print_json_lines, print_selected_human_fields, ListOutputControls,
};
use crate::resolve::resolve_note_argument;
use crate::{
    export_rows, selected_read_permission_filter, AnsiPalette, Cli, CliError, GraphCommand,
    GraphExportFormat, OutputFormat, ResolvedExport,
};
use serde_json::Value;
use vulcan_core::{
    export_graph_with_filter, query_graph_analytics_with_filter,
    query_graph_communities_with_filter, query_graph_components_with_filter,
    query_graph_dead_ends_with_filter, query_graph_hubs_with_filter,
    query_graph_moc_candidates_with_filter, query_graph_path_with_filter, query_graph_trends,
    GraphAnalyticsReport, GraphCommunitiesReport, GraphComponentsReport, GraphDeadEndsReport,
    GraphHubsReport, GraphMocCandidate, GraphMocReport, GraphPathReport, GraphTrendsReport,
    NamedCount, VaultPaths,
};

fn print_graph_path_report(output: OutputFormat, report: &GraphPathReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.path.is_empty() {
                println!(
                    "No resolved path from {} to {}.",
                    report.from_path, report.to_path
                );
            } else {
                println!("{}", report.path.join(" -> "));
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_graph_hubs_report(
    output: OutputFormat,
    report: &GraphHubsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_hub_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph hubs"));
            }
            if visible_notes.is_empty() {
                println!("No graph hubs.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [{} inbound, {} outbound]",
                        note.document_path, note.inbound, note.outbound
                    );
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_moc_report(
    output: OutputFormat,
    report: &GraphMocReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_moc_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("MOC candidates"));
            }
            if visible_notes.is_empty() {
                println!("No MOC candidates.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!(
                        "- {} [score {}, {} inbound, {} outbound]",
                        note.document_path, note.score, note.inbound, note.outbound
                    );
                    if !note.reasons.is_empty() {
                        println!("  {}", note.reasons.join("; "));
                    }
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_dead_ends_report(
    output: OutputFormat,
    report: &GraphDeadEndsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_dead_end_rows(visible_notes);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph dead ends"));
            }
            if visible_notes.is_empty() {
                println!("No dead ends.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for note in visible_notes {
                    println!("- {note}");
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_graph_components_report(
    output: OutputFormat,
    report: &GraphComponentsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_components = paginated_items(&report.components, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = graph_component_rows(visible_components);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph components"));
            }
            if visible_components.is_empty() {
                println!("No components.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for component in visible_components {
                    println!("- size {}: {}", component.size, component.notes.join(", "));
                }
            }
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn print_graph_communities_report(
    output: OutputFormat,
    report: &GraphCommunitiesReport,
    community: Option<usize>,
    orphans: bool,
    bridges: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    let rows = graph_community_rows(report, community, orphans, bridges);
    let visible_rows = paginated_items(&rows, list_controls);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Graph communities"));
            }
            if visible_rows.is_empty() {
                println!("No graph communities.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else if orphans {
                for row in visible_rows {
                    println!(
                        "- {} -> community {} [tag overlap {:.3}]",
                        row["document_path"].as_str().unwrap_or(""),
                        row["closest_community"]
                            .as_u64()
                            .map_or_else(|| "none".to_string(), |id| id.to_string()),
                        row["tag_overlap"].as_f64().unwrap_or(0.0)
                    );
                }
            } else if bridges {
                for row in visible_rows {
                    println!(
                        "- {} [community {}, {} cross-community edges]",
                        row["document_path"].as_str().unwrap_or(""),
                        row["community_id"].as_u64().unwrap_or(0),
                        row["cross_community_edges"].as_u64().unwrap_or(0)
                    );
                }
            } else {
                for row in visible_rows {
                    println!(
                        "- community {}: {} [{} notes, cohesion {:.3}]",
                        row["id"].as_u64().unwrap_or(0),
                        row["label"].as_str().unwrap_or(""),
                        row["size"].as_u64().unwrap_or(0),
                        row["cohesion"].as_f64().unwrap_or(0.0)
                    );
                }
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn print_graph_analytics_report(
    output: OutputFormat,
    report: &GraphAnalyticsReport,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_analytics_rows(report);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("Notes: {}", report.note_count);
            println!("Attachments: {}", report.attachment_count);
            println!("Bases: {}", report.base_count);
            println!("Resolved note links: {}", report.resolved_note_links);
            println!(
                "Confidence: {} EXTRACTED, {} INFERRED, {} AMBIGUOUS",
                report.confidence.extracted,
                report.confidence.inferred,
                report.confidence.ambiguous
            );
            println!(
                "Average outbound links: {:.3}",
                report.average_outbound_links
            );
            println!("Orphan notes: {}", report.orphan_notes);
            print_named_count_section("Top tags", &report.top_tags);
            print_named_count_section("Top properties", &report.top_properties);
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json(report)
        }
    }
}

fn print_graph_trends_report(
    output: OutputFormat,
    report: &GraphTrendsReport,
    list_controls: &ListOutputControls,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = graph_trend_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let visible_points = paginated_items(&report.points, list_controls);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.points.is_empty() {
                println!("No graph trend checkpoints.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for point in visible_points {
                    println!(
                        "- {}: {} notes, {} orphan, {} stale, {} resolved links",
                        point.label,
                        point.note_count,
                        point.orphan_notes,
                        point.stale_notes,
                        point.resolved_links
                    );
                }
            }
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() || export.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

fn print_named_count_section(title: &str, counts: &[NamedCount]) {
    if counts.is_empty() {
        return;
    }
    println!("{title}:");
    for count in counts {
        println!("- {} ({})", count.name, count.count);
    }
}

fn graph_hub_rows(notes: &[vulcan_core::GraphNodeScore]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "total": note.total,
                "confidence": note.confidence,
            })
        })
        .collect()
}

fn graph_moc_rows(notes: &[GraphMocCandidate]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "inbound": note.inbound,
                "outbound": note.outbound,
                "score": note.score,
                "reasons": note.reasons,
            })
        })
        .collect()
}

fn graph_dead_end_rows(notes: &[String]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| serde_json::json!({ "document_path": note }))
        .collect()
}

fn graph_component_rows(components: &[vulcan_core::GraphComponent]) -> Vec<Value> {
    components
        .iter()
        .map(|component| {
            serde_json::json!({
                "size": component.size,
                "notes": component.notes,
            })
        })
        .collect()
}

fn graph_community_rows(
    report: &GraphCommunitiesReport,
    community: Option<usize>,
    orphans: bool,
    bridges: bool,
) -> Vec<Value> {
    if orphans {
        return report
            .orphans
            .iter()
            .map(|orphan| {
                serde_json::json!({
                    "kind": "orphan",
                    "document_path": orphan.document_path,
                    "closest_community": orphan.closest_community,
                    "tag_overlap": orphan.tag_overlap,
                })
            })
            .collect();
    }
    if bridges {
        return report
            .bridges
            .iter()
            .map(|bridge| {
                serde_json::json!({
                    "kind": "bridge",
                    "document_path": bridge.document_path,
                    "community_id": bridge.community_id,
                    "cross_community_edges": bridge.cross_community_edges,
                    "betweenness_score": bridge.betweenness_score,
                })
            })
            .collect();
    }
    report
        .communities
        .iter()
        .filter(|candidate| match community {
            Some(id) => candidate.id == id,
            None => true,
        })
        .map(|community| {
            serde_json::json!({
                "kind": "community",
                "id": community.id,
                "label": community.label,
                "size": community.size,
                "cohesion": community.cohesion,
                "top_nodes": community.top_nodes,
                "boundary_notes": community.boundary_notes,
                "inter_community_edges": community.inter_community_edges,
                "notes": community.notes,
                "persisted": report.persisted,
            })
        })
        .collect()
}

fn graph_analytics_rows(report: &GraphAnalyticsReport) -> Vec<Value> {
    vec![serde_json::json!({
        "note_count": report.note_count,
        "attachment_count": report.attachment_count,
        "base_count": report.base_count,
        "resolved_note_links": report.resolved_note_links,
        "average_outbound_links": report.average_outbound_links,
        "orphan_notes": report.orphan_notes,
        "confidence": report.confidence,
        "top_tags": report.top_tags,
        "top_properties": report.top_properties,
    })]
}

fn graph_trend_rows(report: &GraphTrendsReport) -> Vec<Value> {
    report
        .points
        .iter()
        .map(|point| {
            serde_json::json!({
                "label": point.label,
                "created_at": point.created_at,
                "note_count": point.note_count,
                "orphan_notes": point.orphan_notes,
                "stale_notes": point.stale_notes,
                "resolved_links": point.resolved_links,
            })
        })
        .collect()
}

fn print_graph_export_report(
    output: OutputFormat,
    report: &vulcan_core::GraphExportReport,
    format: GraphExportFormat,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            match format {
                GraphExportFormat::Json => {
                    print_json(report)?;
                }
                GraphExportFormat::Dot => {
                    println!("digraph vault {{");
                    for node in &report.nodes {
                        let label = node.path.trim_end_matches(".md").replace('"', "\\\"");
                        let id = node.path.replace('"', "\\\"");
                        println!("  \"{id}\" [label=\"{label}\"];");
                    }
                    for edge in &report.edges {
                        let src = edge.source.replace('"', "\\\"");
                        let tgt = edge.target.replace('"', "\\\"");
                        println!(
                            "  \"{src}\" -> \"{tgt}\" [confidence=\"{}\", confidence_score=\"{:.3}\"];",
                            edge.confidence.as_str(),
                            edge.confidence_score
                        );
                    }
                    println!("}}");
                }
                GraphExportFormat::Graphml => {
                    println!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
                    println!("<graphml xmlns=\"http://graphml.graphdrawing.org/graphml\">");
                    println!("  <graph id=\"vault\" edgedefault=\"directed\">");
                    for node in &report.nodes {
                        let id = node.path.replace('"', "&quot;");
                        println!("    <node id=\"{id}\"/>");
                    }
                    for (i, edge) in report.edges.iter().enumerate() {
                        let src = edge.source.replace('"', "&quot;");
                        let tgt = edge.target.replace('"', "&quot;");
                        println!(
                            "    <edge id=\"e{i}\" source=\"{src}\" target=\"{tgt}\"><data key=\"confidence\">{}</data><data key=\"confidence_score\">{:.3}</data></edge>",
                            edge.confidence.as_str(),
                            edge.confidence_score
                        );
                    }
                    println!("  </graph>");
                    println!("</graphml>");
                }
            }
            Ok(())
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// template show
// ────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_graph_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &GraphCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        GraphCommand::Path { from, to } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let from = resolve_note_argument(
                paths,
                from.as_deref(),
                interactive_note_selection,
                "from note",
            )?;
            let to =
                resolve_note_argument(paths, to.as_deref(), interactive_note_selection, "to note")?;
            let report = query_graph_path_with_filter(paths, &from, &to, read_filter.as_ref())
                .map_err(CliError::operation)?;
            print_graph_path_report(cli.output, &report)
        }
        GraphCommand::Hubs { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_hubs_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_hubs_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Moc { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_moc_candidates_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_moc_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::DeadEnds { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_dead_ends_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_dead_ends_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Components { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_components_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_components_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Communities {
            community,
            orphans,
            bridges,
            dry_run,
            export,
        } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_communities_with_filter(paths, read_filter.as_ref(), !dry_run)
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_communities_report(
                cli.output,
                &report,
                *community,
                *orphans,
                *bridges,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Stats { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_analytics_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_analytics_report(cli.output, &report, export.as_ref())
        }
        GraphCommand::Trends { limit, export } => {
            let report = query_graph_trends(paths, *limit).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_graph_trends_report(cli.output, &report, list_controls, export.as_ref())
        }
        GraphCommand::Export { format } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = export_graph_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            print_graph_export_report(cli.output, &report, *format)
        }
    }
}
