use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, warn_auto_commit_if_needed, ArtifactCommand, ArtifactHierarchyArg,
    Cli, CliError, OutputFormat,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use vulcan_app::artifact::{
    import_artifact, ArtifactHierarchyAuthority, ArtifactImportReport, ArtifactImportRequest,
};
use vulcan_core::artifact::{inspect_mdaf, MdafArtifact, MdafDiagnostic, MdafRepresentation};
use vulcan_core::{PermissionGuard, VaultPaths};

#[derive(Debug, Serialize)]
struct ArtifactInspectionReport<'a> {
    artifact: &'a Path,
    identity: &'a str,
    representation: MdafRepresentation,
    valid: bool,
    title: Option<&'a str>,
    producer: Option<&'a vulcan_core::artifact::MdafProducer>,
    capabilities: &'a [String],
    derived_from: &'a [String],
    sources: &'a [vulcan_core::artifact::MdafSource],
    members: &'a [vulcan_core::artifact::MdafMember],
    markdown_bytes: Option<usize>,
    source_mappings: usize,
    source_references: usize,
    selector_counts: BTreeMap<&'static str, usize>,
    outline_nodes: usize,
    provenance_activities: usize,
    diagnostics: &'a [MdafDiagnostic],
}

pub(crate) fn handle_artifact_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &ArtifactCommand,
) -> Result<(), CliError> {
    match command {
        ArtifactCommand::Inspect { artifact } => {
            let artifact = inspect_mdaf(artifact).map_err(CliError::operation)?;
            print_inspection(cli.output, &artifact)
        }
        ArtifactCommand::Validate { artifact } => {
            let artifact = inspect_mdaf(artifact).map_err(CliError::operation)?;
            print_inspection(cli.output, &artifact)?;
            if artifact.valid {
                Ok(())
            } else {
                Err(CliError::issues("MDAF validation errors detected"))
            }
        }
        ArtifactCommand::Import {
            artifact,
            destination,
            hierarchy,
            from_level,
            through_level,
            no_navigation,
            dry_run,
            no_commit,
        } => {
            selected_permission_guard(cli, paths)?
                .check_refactor_path(destination)
                .map_err(CliError::operation)?;
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = import_artifact(
                paths,
                &ArtifactImportRequest {
                    artifact: artifact.clone(),
                    destination: destination.clone(),
                    hierarchy: core_hierarchy(*hierarchy),
                    from_level: *from_level,
                    through_level: through_level.unwrap_or(*from_level),
                    navigation: !*no_navigation,
                    dry_run: *dry_run,
                },
            )
            .map_err(CliError::operation)?;
            if !dry_run {
                auto_commit
                    .commit(
                        paths,
                        "artifact-import",
                        &report.changed_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_import(cli.output, &report)
        }
    }
}

fn core_hierarchy(value: ArtifactHierarchyArg) -> ArtifactHierarchyAuthority {
    match value {
        ArtifactHierarchyArg::Markdown => ArtifactHierarchyAuthority::Markdown,
        ArtifactHierarchyArg::Outline => ArtifactHierarchyAuthority::Outline,
    }
}

fn inspection_report(artifact: &MdafArtifact) -> ArtifactInspectionReport<'_> {
    let manifest = artifact.manifest.as_ref();
    ArtifactInspectionReport {
        artifact: &artifact.artifact_path,
        identity: &artifact.identity,
        representation: artifact.representation,
        valid: artifact.valid,
        title: manifest.and_then(|manifest| manifest.title.as_deref()),
        producer: manifest.map(|manifest| &manifest.producer),
        capabilities: manifest.map_or(&[], |manifest| manifest.capabilities.as_slice()),
        derived_from: manifest.map_or(&[], |manifest| manifest.derived_from.as_slice()),
        sources: manifest.map_or(&[], |manifest| manifest.sources.as_slice()),
        members: manifest.map_or(&[], |manifest| manifest.members.as_slice()),
        markdown_bytes: artifact.markdown.as_ref().map(String::len),
        source_mappings: artifact
            .source_map
            .as_ref()
            .map_or(0, |map| map.mappings.len()),
        source_references: artifact
            .source_map
            .as_ref()
            .map_or(0, |map| map.references.len()),
        selector_counts: artifact
            .source_map
            .as_ref()
            .map_or_else(BTreeMap::new, |map| {
                map.mappings
                    .iter()
                    .flat_map(|mapping| &mapping.source.selectors)
                    .chain(
                        map.references
                            .iter()
                            .flat_map(|reference| &reference.target.selectors),
                    )
                    .fold(BTreeMap::new(), |mut counts, selector| {
                        *counts.entry(selector.kind()).or_insert(0) += 1;
                        counts
                    })
            }),
        outline_nodes: artifact
            .outline
            .as_ref()
            .map_or(0, |outline| outline.nodes.len()),
        provenance_activities: artifact
            .provenance
            .as_ref()
            .map_or(0, |provenance| provenance.activities.len()),
        diagnostics: &artifact.diagnostics,
    }
}

fn print_inspection(output: OutputFormat, artifact: &MdafArtifact) -> Result<(), CliError> {
    let report = inspection_report(artifact);
    match output {
        OutputFormat::Json => print_json(&report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!("MDAF: {}", report.artifact.display());
            println!("Identity: {}", report.identity);
            println!("Valid: {}", report.valid);
            if let Some(producer) = report.producer {
                println!("Producer: {} {}", producer.name, producer.version);
            }
            println!("Members: {}", report.members.len());
            println!("Sources: {}", report.sources.len());
            for source in report.sources {
                println!("- {} ({})", source.id, source.media_type);
            }
            println!("Source mappings: {}", report.source_mappings);
            if !report.selector_counts.is_empty() {
                println!(
                    "Selectors: {}",
                    report
                        .selector_counts
                        .iter()
                        .map(|(kind, count)| format!("{kind}={count}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("Outline nodes: {}", report.outline_nodes);
            println!("Provenance activities: {}", report.provenance_activities);
            for diagnostic in report.diagnostics {
                println!(
                    "{:?} {}: {}",
                    diagnostic.severity, diagnostic.code, diagnostic.message
                );
            }
            Ok(())
        }
    }
}

fn print_import(output: OutputFormat, report: &ArtifactImportReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} MDAF {} into {} as {:?} hierarchy ({} notes, {} assets)",
                if report.dry_run {
                    "Would import"
                } else {
                    "Imported"
                },
                report.artifact_identity,
                report.destination_root,
                report.hierarchy,
                report.notes.len(),
                report.assets.len()
            );
            for note in &report.notes {
                println!("{} ({})", note.path, note.title);
            }
            for diagnostic in &report.diagnostics {
                println!("{}: {}", diagnostic.code, diagnostic.message);
            }
            Ok(())
        }
    }
}
