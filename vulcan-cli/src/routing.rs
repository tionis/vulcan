//! Routing routable reads to the resident daemon (Roadmap 10.5, Part B of
//! `docs/specs/index-ownership-and-client-routing.md`).
//!
//! `--route direct` (the default) never contacts the daemon. `auto` asks the
//! daemon when its runtime record exists and runs directly whenever it does
//! not answer; reads have no effect beyond rebuildable cache refreshes, so a
//! fallback after any failure is safe. `daemon` reports every failure instead.
//! A routed command defers this process's own cache refresh until it knows
//! the read runs here.

use vulcan_app::routed::dql::DqlRequest;
use vulcan_app::routed::graph::{GraphAnswer, GraphRequest};
use vulcan_app::routed::ls::LsRequest;
use vulcan_app::routed::query::QueryRequest;
use vulcan_app::routed::tasks::TasksQueryRequest;
use vulcan_core::dql::DqlQueryResult;
use vulcan_core::search::{SearchQuery, SearchReport};
use vulcan_core::tasks::TasksQueryResult;
use vulcan_core::{QueryReport, VaultPaths};
use vulcan_daemon::process::{
    request_routed_dql, request_routed_graph, request_routed_ls, request_routed_query,
    request_routed_search, request_routed_tasks_query, DaemonProcessContext, RoutedClientError,
};
use vulcan_daemon::routed_reads::{
    RoutedDql, RoutedGraph, RoutedLs, RoutedQuery, RoutedSearch, RoutedTasksQuery,
};

use crate::cli::{Command, GraphCommand, RouteMode, SearchMode, TasksCommand};
use crate::{Cli, CliError};

fn route_mode(cli: &Cli) -> RouteMode {
    cli.route.unwrap_or(RouteMode::Direct)
}

/// Whether the command is a read the daemon can answer.
fn is_routable(command: &Command) -> bool {
    match command {
        // DSL, JSON, and DQL queries; not the field catalog.
        Command::Query {
            dsl,
            json,
            list_fields: false,
            ..
        } => dsl.is_some() || json.is_some(),
        // Hybrid search embeds the query with this process's provider.
        Command::Search { mode, .. } => *mode == SearchMode::Keyword,
        Command::Ls { .. }
        | Command::Tasks {
            command: TasksCommand::Query { .. },
        }
        | Command::Graph {
            command:
                GraphCommand::Stats { .. } | GraphCommand::Hubs { .. } | GraphCommand::Path { .. },
        } => true,
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
    route(cli, paths, |context, refresh| {
        request_routed_query(
            context,
            paths.vault_root(),
            &RoutedQuery {
                request: request.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
        .map(QueryReport::from)
    })
}

/// The daemon's answer to a DQL query, or `None` when it should run here.
pub(crate) fn route_dql(
    cli: &Cli,
    paths: &VaultPaths,
    request: &DqlRequest,
) -> Result<Option<DqlQueryResult>, CliError> {
    route(cli, paths, |context, refresh| {
        request_routed_dql(
            context,
            paths.vault_root(),
            &RoutedDql {
                request: request.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
    })
}

/// The daemon's answer to `tasks query`, or `None` when it should run here.
pub(crate) fn route_tasks_query(
    cli: &Cli,
    paths: &VaultPaths,
    request: &TasksQueryRequest,
) -> Result<Option<TasksQueryResult>, CliError> {
    route(cli, paths, |context, refresh| {
        request_routed_tasks_query(
            context,
            paths.vault_root(),
            &RoutedTasksQuery {
                request: request.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
    })
}

/// The daemon's answer to a whole-graph report, or `None` when it should
/// run here.
pub(crate) fn route_graph(
    cli: &Cli,
    paths: &VaultPaths,
    request: &GraphRequest,
) -> Result<Option<GraphAnswer>, CliError> {
    route(cli, paths, |context, refresh| {
        request_routed_graph(
            context,
            paths.vault_root(),
            &RoutedGraph {
                request: request.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
    })
}

/// The daemon's answer to `ls`, or `None` when it should run here.
pub(crate) fn route_ls(
    cli: &Cli,
    paths: &VaultPaths,
    request: &LsRequest,
) -> Result<Option<QueryReport>, CliError> {
    route(cli, paths, |context, refresh| {
        request_routed_ls(
            context,
            paths.vault_root(),
            &RoutedLs {
                request: request.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
        .map(QueryReport::from)
    })
}

/// The daemon's answer to a keyword `search`, or `None` when it should run
/// here.
pub(crate) fn route_search(
    cli: &Cli,
    paths: &VaultPaths,
    query: &SearchQuery,
) -> Result<Option<SearchReport>, CliError> {
    route(cli, paths, |context, refresh| {
        request_routed_search(
            context,
            paths.vault_root(),
            &RoutedSearch {
                query: query.clone(),
                permissions: cli.permissions.clone(),
                refresh,
            },
        )
    })
}

/// Ask the daemon with `ask` (given whether the caller wants a fresh
/// index) unless the mode is `direct`, and apply the fallback rules.
fn route<T>(
    cli: &Cli,
    paths: &VaultPaths,
    ask: impl FnOnce(&DaemonProcessContext, bool) -> Result<T, RoutedClientError>,
) -> Result<Option<T>, CliError> {
    let mode = route_mode(cli);
    if mode == RouteMode::Direct {
        return Ok(None);
    }
    let answer = DaemonProcessContext::user_default()
        .map_err(|error| RoutedClientError::Transport(error.to_string()))
        .and_then(|context| ask(&context, crate::refresh_requested(paths, cli)));
    match (answer, mode) {
        (Ok(report), _) => Ok(Some(report)),
        // Running it here fails the same way; report the daemon's error.
        (Err(RoutedClientError::Failed(message)), _) => Err(CliError::operation(message)),
        (Err(_), RouteMode::Auto) => Ok(None),
        (Err(error), _) => Err(CliError::daemon_unavailable(error)),
    }
}
