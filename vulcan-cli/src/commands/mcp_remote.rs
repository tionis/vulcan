use crate::output::print_json;
use crate::{
    mcp, Cli, CliError, McpCommand, McpConnectionsCommand, McpRemoteCommand, McpToolPackArg,
    OutputFormat,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;
use vulcan_core::{resolve_permission_profile, PermissionGrant, VaultPaths};
use vulcan_daemon::mcp_remote::{
    AddMcpRemoteRequest, McpRemoteAuthentication, McpRemoteDefinition, McpRemoteId, McpRemoteVault,
    UpdateMcpRemoteRequest,
};
use vulcan_daemon::mcp_state::{ConnectionGrantReport, McpAuthorizationStore};
use vulcan_daemon::process::DaemonProcessContext;
use vulcan_daemon::registry::{WikiId, WikiRegistration};

#[derive(Debug, Serialize)]
struct RemoteMutationReport {
    dry_run: bool,
    remote: McpRemoteDefinition,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    revoked_connections: Vec<ConnectionGrantReport>,
}

pub(crate) fn handle_mcp_command(cli: &Cli, command: &McpCommand) -> Result<(), CliError> {
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    match command {
        McpCommand::Remote { command } => handle_remote(cli, &context, command),
        McpCommand::Connections { command } => handle_connections(cli, &context, command),
    }
}

#[allow(clippy::too_many_lines)]
fn handle_remote(
    cli: &Cli,
    context: &DaemonProcessContext,
    command: &McpRemoteCommand,
) -> Result<(), CliError> {
    match command {
        McpRemoteCommand::Init {
            name,
            public_url,
            identity,
            wiki,
            bind,
            ceiling_profile,
            default_profile,
            tool_pack,
            dry_run,
        } => {
            let registration = resolve_registration(cli, context, wiki.as_deref())?;
            validate_profiles(&registration, ceiling_profile, default_profile)?;
            let id = McpRemoteId::parse(name).map_err(CliError::operation)?;
            let bind = match bind {
                Some(bind) => bind.clone(),
                None => choose_available_bind(context)?,
            };
            let remote = context
                .registry
                .add_mcp_remote(
                    AddMcpRemoteRequest {
                        id,
                        bind,
                        public_url: public_url.clone(),
                        authentication: McpRemoteAuthentication::IndieAuth {
                            identity: identity.clone(),
                        },
                        vaults: vec![McpRemoteVault {
                            wiki_id: registration.id,
                            ceiling_profile: ceiling_profile.clone(),
                            default_profile: default_profile.clone(),
                            tool_packs: tool_pack_names(tool_pack),
                        }],
                    },
                    *dry_run,
                )
                .map_err(CliError::operation)?;
            print_remote_mutation(cli.output, *dry_run, "initialize", remote, Vec::new())
        }
        McpRemoteCommand::List => {
            let remotes = context
                .registry
                .list_mcp_remotes()
                .map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                return print_json(&remotes);
            }
            if remotes.is_empty() {
                println!("No named MCP remotes configured.");
            }
            for remote in remotes {
                println!("{}\t{}\t{}", remote.id, remote.bind, remote.public_url);
            }
            Ok(())
        }
        McpRemoteCommand::Show { name } => {
            let remote = show_remote(context, name)?;
            print_remote(cli.output, &remote)
        }
        McpRemoteCommand::Set {
            name,
            wiki,
            add_wiki,
            remove_wiki,
            bind,
            public_url,
            identity,
            ceiling_profile,
            default_profile,
            tool_pack,
            dry_run,
        } => {
            let id = McpRemoteId::parse(name).map_err(CliError::operation)?;
            let _runtime_lock = if *dry_run {
                None
            } else {
                Some(mcp::acquire_named_remote_runtime_lock(
                    &context.state_root.join("mcp-remotes").join(id.as_str()),
                    &context
                        .registry
                        .show_mcp_remote(&id)
                        .map_err(CliError::operation)?,
                )?)
            };
            let existing = context
                .registry
                .show_mcp_remote(&id)
                .map_err(CliError::operation)?;
            let mut vaults = existing.vaults.clone();
            let changing_vault_policy =
                ceiling_profile.is_some() || default_profile.is_some() || !tool_pack.is_empty();
            let removed_wiki = remove_wiki
                .as_ref()
                .map(WikiId::parse)
                .transpose()
                .map_err(CliError::operation)?;
            if let Some(removed) = removed_wiki.as_ref() {
                if changing_vault_policy
                    || wiki.is_some()
                    || add_wiki.is_some()
                    || bind.is_some()
                    || public_url.is_some()
                    || identity.is_some()
                {
                    return Err(CliError::operation(
                        "--remove-wiki must be a separate remote set operation",
                    ));
                }
                if vaults.len() == 1 {
                    return Err(CliError::operation(
                        "a named remote must expose at least one wiki; use `mcp remote remove` to remove the instance",
                    ));
                }
                let before = vaults.len();
                vaults.retain(|vault| &vault.wiki_id != removed);
                if vaults.len() == before {
                    return Err(CliError::operation(format!(
                        "wiki `{removed}` is not exposed by remote `{id}`"
                    )));
                }
            } else if let Some(added) = add_wiki.as_ref() {
                let registration = resolve_registration(cli, context, Some(added))?;
                if vaults.iter().any(|vault| vault.wiki_id == registration.id) {
                    return Err(CliError::operation(format!(
                        "wiki `{}` is already exposed by remote `{id}`",
                        registration.id
                    )));
                }
                let ceiling = ceiling_profile.as_deref().unwrap_or("readonly");
                let default = default_profile.as_deref().unwrap_or("readonly");
                validate_profiles(&registration, ceiling, default)?;
                let packs = if tool_pack.is_empty() {
                    tool_pack_names(&[
                        McpToolPackArg::NotesRead,
                        McpToolPackArg::Search,
                        McpToolPackArg::Status,
                    ])
                } else {
                    tool_pack_names(tool_pack)
                };
                vaults.push(McpRemoteVault {
                    wiki_id: registration.id,
                    ceiling_profile: ceiling.to_string(),
                    default_profile: default.to_string(),
                    tool_packs: packs,
                });
            } else if changing_vault_policy || wiki.is_some() {
                let selected = match wiki {
                    Some(wiki) => WikiId::parse(wiki).map_err(CliError::operation)?,
                    None if vaults.len() == 1 => vaults[0].wiki_id.clone(),
                    None => {
                        return Err(CliError::operation(
                            "multi-vault remote policy updates require --wiki <id>",
                        ));
                    }
                };
                let vault = vaults
                    .iter_mut()
                    .find(|vault| vault.wiki_id == selected)
                    .ok_or_else(|| {
                        CliError::operation(format!(
                            "wiki `{selected}` is not exposed by remote `{id}`"
                        ))
                    })?;
                if let Some(profile) = ceiling_profile {
                    vault.ceiling_profile.clone_from(profile);
                }
                if let Some(profile) = default_profile {
                    vault.default_profile.clone_from(profile);
                }
                if !tool_pack.is_empty() {
                    vault.tool_packs = tool_pack_names(tool_pack);
                }
                let registration = context
                    .registry
                    .show(&selected)
                    .map_err(CliError::operation)?
                    .registration;
                validate_profiles(
                    &registration,
                    &vault.ceiling_profile,
                    &vault.default_profile,
                )?;
            }
            let update = UpdateMcpRemoteRequest {
                bind: bind.clone(),
                public_url: public_url.clone(),
                authentication: identity.as_ref().map(|identity| {
                    McpRemoteAuthentication::IndieAuth {
                        identity: identity.clone(),
                    }
                }),
                vaults: Some(vaults),
            };
            context
                .registry
                .update_mcp_remote(&id, update.clone(), true)
                .map_err(CliError::operation)?;
            let revoked = if let Some(removed) = removed_wiki.as_ref() {
                McpAuthorizationStore::at(&context.state_root)
                    .revoke_remote_wiki_grants(&id, removed, unix_timestamp()?, *dry_run)
                    .map_err(CliError::operation)?
            } else {
                Vec::new()
            };
            let remote = context
                .registry
                .update_mcp_remote(&id, update, *dry_run)
                .map_err(CliError::operation)?;
            print_remote_mutation(cli.output, *dry_run, "update", remote, revoked)
        }
        McpRemoteCommand::Run { name } => {
            let remote = show_remote(context, name)?;
            mcp::run_named_mcp_remote(context, &remote)
        }
        McpRemoteCommand::Remove {
            name,
            preserve_grants,
            dry_run,
        } => {
            let id = McpRemoteId::parse(name).map_err(CliError::operation)?;
            let _runtime_lock = if *dry_run {
                None
            } else {
                Some(mcp::acquire_named_remote_runtime_lock(
                    &context.state_root.join("mcp-remotes").join(id.as_str()),
                    &context
                        .registry
                        .show_mcp_remote(&id)
                        .map_err(CliError::operation)?,
                )?)
            };
            let remote = context
                .registry
                .show_mcp_remote(&id)
                .map_err(CliError::operation)?;
            let revoked = if *preserve_grants {
                Vec::new()
            } else {
                McpAuthorizationStore::at(&context.state_root)
                    .revoke_remote_grants(&id, unix_timestamp()?, *dry_run)
                    .map_err(CliError::operation)?
            };
            context
                .registry
                .remove_mcp_remote(&id, *dry_run)
                .map_err(CliError::operation)?;
            print_remote_mutation(cli.output, *dry_run, "remove", remote, revoked)
        }
    }
}

fn handle_connections(
    cli: &Cli,
    context: &DaemonProcessContext,
    command: &McpConnectionsCommand,
) -> Result<(), CliError> {
    let store = McpAuthorizationStore::at(&context.state_root);
    match command {
        McpConnectionsCommand::List { remote } => {
            let remote = remote
                .as_ref()
                .map(McpRemoteId::parse)
                .transpose()
                .map_err(CliError::operation)?;
            let reports = store
                .list_grants(remote.as_ref())
                .map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                return print_json(&reports);
            }
            if reports.is_empty() {
                println!("No approved MCP connections.");
            }
            for report in reports {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    report.id,
                    report.remote_id,
                    report.subject,
                    report.permission_profile,
                    if report.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    }
                );
            }
            Ok(())
        }
        McpConnectionsCommand::Show { id } => {
            let id = parse_ulid(id, "connection")?;
            let report = store.show_grant(id).map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                print_json(&report)
            } else {
                println!("Connection: {}", report.id);
                println!("Remote: {}", report.remote_id);
                println!("Subject: {}", report.subject);
                println!("Vault: {}", report.wiki_id);
                println!("Permission profile: {}", report.permission_profile);
                println!("Tool packs: {}", report.tool_packs.join(", "));
                println!("Scopes: {}", report.scopes.join(" "));
                println!("Expires: {}", report.expires_at);
                println!(
                    "State: {}",
                    if report.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    }
                );
                Ok(())
            }
        }
        McpConnectionsCommand::Revoke { id, dry_run } => {
            let id = parse_ulid(id, "connection")?;
            let report = store
                .revoke_grant(id, unix_timestamp()?, *dry_run)
                .map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                print_json(&serde_json::json!({"dry_run": dry_run, "connection": report}))
            } else {
                println!(
                    "{} connection {}",
                    if *dry_run { "Would revoke" } else { "Revoked" },
                    report.id
                );
                Ok(())
            }
        }
    }
}

fn show_remote(
    context: &DaemonProcessContext,
    name: &str,
) -> Result<McpRemoteDefinition, CliError> {
    let id = McpRemoteId::parse(name).map_err(CliError::operation)?;
    context
        .registry
        .show_mcp_remote(&id)
        .map_err(CliError::operation)
}

fn resolve_registration(
    cli: &Cli,
    context: &DaemonProcessContext,
    wiki: Option<&str>,
) -> Result<WikiRegistration, CliError> {
    match wiki {
        Some(wiki) => context
            .registry
            .show(&WikiId::parse(wiki).map_err(CliError::operation)?)
            .map(|report| report.registration)
            .map_err(CliError::operation),
        None => context
            .registry
            .find_by_path(&cli.vault)
            .map_err(|_| {
                CliError::operation(
                    "the current vault is not registered; run `vulcan vault add <id> <path>` or pass --wiki",
                )
            }),
    }
}

fn validate_profiles(
    registration: &WikiRegistration,
    ceiling: &str,
    default: &str,
) -> Result<(), CliError> {
    if ceiling == "unrestricted" {
        return Err(CliError::operation(
            "named MCP remote ceilings cannot be `unrestricted`",
        ));
    }
    let paths = VaultPaths::new(&registration.path);
    let ceiling = resolve_permission_profile(&paths, Some(ceiling)).map_err(CliError::operation)?;
    let default = resolve_permission_profile(&paths, Some(default)).map_err(CliError::operation)?;
    let default_grant = PermissionGrant::from_profile(&default.profile);
    let ceiling_grant = PermissionGrant::from_profile(&ceiling.profile);
    if !default_grant.is_subset_of(&ceiling_grant) {
        return Err(CliError::operation(format!(
            "default permission profile `{}` exceeds remote ceiling `{}`",
            default.name, ceiling.name
        )));
    }
    Ok(())
}

fn choose_available_bind(context: &DaemonProcessContext) -> Result<String, CliError> {
    let used = context
        .registry
        .list_mcp_remotes()
        .map_err(CliError::operation)?
        .into_iter()
        .map(|remote| remote.bind)
        .collect::<BTreeSet<_>>();
    for port in 8765..=8865 {
        let bind = format!("127.0.0.1:{port}");
        if used.contains(&bind) {
            continue;
        }
        let address = bind.parse::<SocketAddr>().expect("generated bind is valid");
        if TcpListener::bind(address).is_ok() {
            return Ok(bind);
        }
    }
    Err(CliError::operation(
        "no available loopback MCP port found in 8765-8865; pass --bind",
    ))
}

fn tool_pack_names(packs: &[McpToolPackArg]) -> Vec<String> {
    packs
        .iter()
        .map(|pack| match pack {
            McpToolPackArg::NotesRead => "notes-read",
            McpToolPackArg::Search => "search",
            McpToolPackArg::Status => "status",
            McpToolPackArg::Graph => "graph",
            McpToolPackArg::Custom => "custom",
            McpToolPackArg::Daily => "daily",
            McpToolPackArg::Tasks => "tasks",
            McpToolPackArg::NotesWrite => "notes-write",
            McpToolPackArg::NotesManage => "notes-manage",
            McpToolPackArg::Web => "web",
            McpToolPackArg::Config => "config",
            McpToolPackArg::Index => "index",
            McpToolPackArg::Sync => "sync",
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn print_remote(output: OutputFormat, remote: &McpRemoteDefinition) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(remote);
    }
    println!("Remote: {}", remote.id);
    println!("MCP URL: {}", remote.public_url);
    println!("Bind: {}", remote.bind);
    println!("Instance: {}", remote.instance_id);
    for vault in &remote.vaults {
        println!("Vault: {}", vault.wiki_id);
        println!("Ceiling: {}", vault.ceiling_profile);
        println!("Default consent: {}", vault.default_profile);
        println!("Tool packs: {}", vault.tool_packs.join(", "));
    }
    Ok(())
}

fn print_remote_mutation(
    output: OutputFormat,
    dry_run: bool,
    action: &str,
    remote: McpRemoteDefinition,
    revoked_connections: Vec<ConnectionGrantReport>,
) -> Result<(), CliError> {
    let report = RemoteMutationReport {
        dry_run,
        remote,
        revoked_connections,
    };
    if output == OutputFormat::Json {
        return print_json(&report);
    }
    let action = match (dry_run, action) {
        (true, "initialize") => "Would initialize",
        (false, "initialize") => "Initialized",
        (true, "update") => "Would update",
        (false, "update") => "Updated",
        (true, "remove") => "Would remove",
        (false, "remove") => "Removed",
        _ => "Changed",
    };
    println!("{action} named MCP remote `{}`", report.remote.id);
    println!("MCP URL: {}", report.remote.public_url);
    if !report.revoked_connections.is_empty() {
        println!("Revoked connections: {}", report.revoked_connections.len());
    }
    Ok(())
}

fn parse_ulid(value: &str, label: &str) -> Result<Ulid, CliError> {
    value
        .parse::<Ulid>()
        .map_err(|error| CliError::operation(format!("invalid {label} ID `{value}`: {error}")))
}

fn unix_timestamp() -> Result<u64, CliError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(CliError::operation)
}
