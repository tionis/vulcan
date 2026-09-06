use crate::output::print_json;
use crate::{selected_read_permission_filter, Cli, CliError, MdbaseCommand, OutputFormat};
use vulcan_app::mdbase::{
    build_mdbase_contracts_report, build_mdbase_read_report, build_mdbase_status_report,
    build_mdbase_types_report, build_mdbase_validate_report, MdbaseContractsReport,
    MdbaseReadReport, MdbaseStatusReport, MdbaseTypesReport, MdbaseValidateReport,
};
use vulcan_core::mdbase::{MdbaseCompleteRecord, MdbaseDiagnosticLevel, MdbaseOperationResult};
use vulcan_core::VaultPaths;

pub(crate) fn handle_mdbase_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &MdbaseCommand,
) -> Result<(), CliError> {
    let filter = selected_read_permission_filter(cli, paths)?;
    match command {
        MdbaseCommand::Status => print_status(
            cli.output,
            &build_mdbase_status_report(paths, filter.as_ref())?,
        ),
        MdbaseCommand::Types => print_types(
            cli.output,
            &build_mdbase_types_report(paths, filter.as_ref())?,
        ),
        MdbaseCommand::Contracts => print_contracts(
            cli.output,
            &build_mdbase_contracts_report(paths, filter.as_ref())?,
        ),
        MdbaseCommand::Validate { path } => print_validate(
            cli.output,
            &build_mdbase_validate_report(paths, path.as_deref(), filter.as_ref())?,
        ),
        MdbaseCommand::Read { path, source } => print_read(
            cli.output,
            &build_mdbase_read_report(paths, path, *source, filter.as_ref())?,
        ),
    }
}

fn print_status(output: OutputFormat, report: &MdbaseStatusReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&MdbaseOperationResult::new(
            report.valid,
            report,
            report.diagnostics.clone(),
        ));
    }
    println!("Collection: {}", report.collection_root);
    println!("Spec:       {}", report.spec_version);
    println!("Records:    {}", report.records);
    println!("Types:      {}", report.types);
    println!("Contracts:  {}", report.contracts);
    println!("Valid:      {}", report.valid);
    print_diagnostics(&report.diagnostics);
    Ok(())
}

fn print_types(output: OutputFormat, report: &MdbaseTypesReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        let valid = report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error);
        return print_json(&MdbaseOperationResult::new(
            valid,
            report,
            report.diagnostics.clone(),
        ));
    }
    for definition in &report.types {
        println!("{}\t{}", definition.name, definition.path);
    }
    print_diagnostics(&report.diagnostics);
    Ok(())
}

fn print_contracts(output: OutputFormat, report: &MdbaseContractsReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        let valid = report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error);
        return print_json(&MdbaseOperationResult::new(
            valid,
            report,
            report.diagnostics.clone(),
        ));
    }
    for entry in &report.contracts {
        println!(
            "{}@{}\t{}\t{} implementation(s)",
            entry.contract.identity.id,
            entry.contract.identity.version,
            entry.contract.path,
            entry.implementations.len()
        );
    }
    print_diagnostics(&report.diagnostics);
    Ok(())
}

fn print_validate(output: OutputFormat, report: &MdbaseValidateReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        let mut diagnostics = report.diagnostics.clone();
        diagnostics.extend(
            report
                .records
                .iter()
                .flat_map(|record| record.diagnostics.iter().cloned()),
        );
        return print_json(&MdbaseOperationResult::new(
            report.valid,
            report,
            diagnostics,
        ));
    }
    println!("Valid: {}", report.valid);
    for record in &report.records {
        println!(
            "{}\t{}\t{}",
            if record.valid { "valid" } else { "invalid" },
            record.path,
            record.types.join(",")
        );
        print_diagnostics(&record.diagnostics);
    }
    print_diagnostics(&report.diagnostics);
    Ok(())
}

fn print_read(output: OutputFormat, report: &MdbaseReadReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&MdbaseOperationResult::new(
            report.valid,
            MdbaseCompleteRecord::from(&report.record),
            report.diagnostics.clone(),
        ));
    }
    println!("Path:     {}", report.record.path);
    println!("Revision: {}", report.record.revision);
    println!("Types:    {}", report.record.types.join(", "));
    println!("Valid:    {}", report.valid);
    println!("\n{}", report.record.body);
    print_diagnostics(&report.diagnostics);
    Ok(())
}

fn print_diagnostics(diagnostics: &[vulcan_core::mdbase::MdbaseDiagnostic]) {
    for diagnostic in diagnostics {
        println!(
            "- {:?} {}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}
