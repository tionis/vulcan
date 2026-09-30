# Named MCP Remotes

Use `vulcan --vault /path/to/vault mcp` for local stdio, or add
`--transport http --bind 127.0.0.1:8765` for local HTTP. Neither requires a daemon,
named remote, or browser login. Named remotes provide HTTPS-facing OAuth access
for hosted clients; they require a build with the `oauth` feature.

## Start with read-only access

Register a Markdown directory, preview the definition, then apply it:

```sh
vulcan vault add personal /path/to/vault --no-sync
vulcan mcp remote init personal-agent --wiki personal --public-url https://wiki.example.com/mcp --identity https://example.com/ --dry-run
vulcan mcp remote init personal-agent --wiki personal --public-url https://wiki.example.com/mcp --identity https://example.com/
vulcan mcp remote show personal-agent
vulcan mcp remote run personal-agent
```

The defaults are a loopback bind, the `readonly` ceiling and consent profile, and
the `notes-read,search,status` packs. `--identity` is your canonical HTTPS IndieAuth
identity, not the MCP client's identity. Discovery does not follow redirects.

Forward the exact public MCP path, its `/operations/<id>` subpath,
`/.well-known/oauth-protected-resource/mcp`,
`/.well-known/oauth-authorization-server/mcp`, and `/oauth/*` to the loopback
listener reported by `show`. These metadata examples assume `/mcp` as the resource
path. Forward length-delimited request bodies with a single `Content-Length`;
chunked request transfer encoding is unsupported. HTTPS ingress supplies transport
security; Vulcan still authenticates and authorizes each connection.

The human signs in through IndieAuth, then separately approves a vault, permission
profile, eligible tool packs, scopes, and expiry on Vulcan's consent page. Packs
select tools; permission profiles constrain authority. The server ceiling is the
maximum, not an automatic grant. A ceiling cannot be `unrestricted`.

For writable daily/task workflows, review `daily-wiki-agent` and the explicit pack
configuration in [ChatGPT MCP setup](chatgpt-mcp.md), also available through
`vulcan help chatgpt-mcp`.

## Multiple instances and vaults

Different named remotes can run concurrently with distinct loopback binds and
non-conflicting public URLs. Use separate foreground processes, or
`vulcan daemon start --detach` to host all configured remotes. Daemon status reports
`listener.mcp-remotes`. A foreground process and the daemon cannot own the same
instance simultaneously.

Stop the affected listener before `remote set`, `remote remove`, or credential
migration. For resident listeners, stop the daemon; restart it after definition
changes. To expose another registered vault through one URL:

```sh
vulcan mcp remote set personal-agent --add-wiki team --dry-run
vulcan mcp remote set personal-agent --add-wiki team
vulcan mcp remote set personal-agent --wiki team --tool-pack notes-read,search --dry-run
```

Review the last preview and apply without `--dry-run` when intended. `--wiki`
selects the existing vault whose ceiling/default/packs change. Both foreground
and resident hosting support multiple vaults, but each approval and MCP session
is bound to one explicitly selected vault. `--remove-wiki <id>` revokes that
vault's grants and refresh families and cannot remove the final vault.

## Inspect and revoke approvals

```sh
vulcan mcp connections list --remote personal-agent
vulcan mcp connections show <grant-id>
vulcan mcp connections revoke <grant-id> --dry-run
vulcan mcp connections revoke <grant-id>
```

Revocation also retires sessions and closes their SSE streams. Access tokens are
short-lived; refresh tokens rotate. Requests and refreshes revalidate the approved
snapshot against current policy. A connection used or refreshed under narrower
permissions stays narrowed even if the profile is later widened: expansion needs
fresh consent. `remote remove` revokes connections by default; retained grants
with `--preserve-grants` remain unusable without their original instance.

## Configuration, credentials, and recovery

Definitions are device-global in Vulcan's user configuration, not in either vault
config file. They reference registered vaults and permission profiles; profiles
remain in shared `.vulcan/config.toml` or local `.vulcan/config.local.toml`.
Grant, refresh, revocation, and operation state lives outside the rebuildable
SQLite cache in device-local operational storage. None of this is vault sync state.

Issuer keys and confidential client secrets use instance-bound `file_v1`
SecretStore references. Inspection reports references, never secret values.
This is protected-file custody, not encryption, a native keychain, or hardware
custody. Public OAuth clients need no stored client secret. Older named remotes
require explicit migration while stopped:

```sh
vulcan mcp remote migrate-credentials personal-agent --dry-run
vulcan mcp remote migrate-credentials personal-agent
```

Preview inspects metadata without reading secret bytes or writing state. Applying
preserves key bytes, client identities, and grants, verifies copies, and retains
legacy issuer files as inactive protected recovery material. Missing or changed
established credentials fail closed against separate fingerprint receipts.
Restore exact credentials from a reviewed device backup, or migrate from an
unchanged retained legacy source. Never delete receipts, replace keys, or recreate
the remote as an implicit repair. Direct HTTP credentials are not migrated.

A hosted write may finish after a response timeout or cancellation. Inspect its
returned `operation_id` and `status_path` with `GET <public-origin><status_path>`
and a current bearer token for the same connection grant before retrying. Known
terminal outcomes are retained for 30 days, unknown outcomes for 90 days. If the
outcome is unknown or expired, inspect the vault before deciding whether to retry.
An operation ledger is not an idempotency guarantee or a Git auto-commit feature.

See [configuration reference](../reference/config.md) for storage conventions and
[implementation acceptance](../specs/named-mcp-acceptance.md) for tested boundaries.
Real ChatGPT account/public-ingress acceptance is a separate deployment check.
