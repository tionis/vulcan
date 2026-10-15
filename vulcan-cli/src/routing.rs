//! Routing routable reads to the resident daemon (Roadmap 10.5, Part B of
//! `docs/specs/index-ownership-and-client-routing.md`).
//!
//! `--route direct` (the default) never contacts the daemon. `auto` asks the
//! daemon when its runtime record exists and runs directly whenever it does
//! not answer; reads have no effect beyond rebuildable cache refreshes, so a
//! fallback after any failure is safe. `daemon` reports every failure instead.
//! A routed command defers this process's own cache refresh until it knows
//! the read runs here.

use vulcan_app::routed::query::QueryRequest;
use vulcan_core::{QueryReport, VaultPaths};
use vulcan_daemon::process::{request_routed_query, DaemonProcessContext, RoutedClientError};
use vulcan_daemon::routed_reads::RoutedQuery;

use crate::cli::{Command, QueryEngineArg, RouteMode};
use crate::{Cli, CliError};

fn route_mode(cli: &Cli) -> RouteMode {
    cli.route.unwrap_or(RouteMode::Direct)
}

/// Whether the command is a read the daemon can answer.
fn is_routable(command: &Command) -> bool {
    match command {
        Command::Query {
            dsl,
            json,
            list_fields: false,
            engine,
            ..
        } => {
            let dql = match engine {
                QueryEngineArg::Dql => true,
                QueryEngineArg::Dsl => false,
                QueryEngineArg::Auto => dsl
                    .as_deref()
                    .is_some_and(crate::commands::query::looks_like_dql),
            };
            !dql && (dsl.is_some() || json.is_some())
        }
        _ => false,
    }
}

/// Whether the command's cache refresh waits for the routing decision.
pub(crate) fn defers_refresh(cli: &Cli) -> bool {
    route_mode(cli) != RouteMode::Direct && is_routable(&cli.command)
}

/// The daemon's answer to `request`, or `None` when it should run here.
pub(crate) fn route_query(
    cli: &Cli,
    paths: &VaultPaths,
    request: &QueryRequest,
) -> Result<Option<QueryReport>, CliError> {
    let mode = route_mode(cli);
    if mode == RouteMode::Direct {
        return Ok(None);
    }
    let answer = DaemonProcessContext::user_default()
        .map_err(|error| RoutedClientError::Transport(error.to_string()))
        .and_then(|context| {
            request_routed_query(
                &context,
                paths.vault_root(),
                &RoutedQuery {
                    request: request.clone(),
                    permissions: cli.permissions.clone(),
                    refresh: crate::refresh_requested(paths, cli),
                },
            )
        });
    match (answer, mode) {
        (Ok(report), _) => Ok(Some(report.into())),
        // Running it here fails the same way; report the daemon's error.
        (Err(RoutedClientError::Failed(message)), _) => Err(CliError::operation(message)),
        (Err(_), RouteMode::Auto) => Ok(None),
        (Err(error), _) => Err(CliError::daemon_unavailable(error)),
    }
}
