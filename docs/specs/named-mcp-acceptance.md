# Named MCP acceptance audit

Audit date: 2026-09-30. Scope: Roadmap 10.10 and its MCP hosting requirements in 10.7.6.
This is an evidence record, not a declaration that every dependency or platform check is complete.

## Implementation and verification evidence

The default workspace suite passes after `383832cb`, including the live foreground/resident
regression. Workspace/all-target linting and no-default CLI/all-target linting pass. Additional
no-default CLI runs pass 13 MCP unit tests and 40 MCP CLI smoke tests. Ignored workspace tests
remain ignored; these results do not establish native Windows execution or live ChatGPT acceptance.

| Requirement | Current implementation and covering evidence |
| --- | --- |
| Client-owned local stdio and loopback HTTP, without an account or resident daemon | `vulcan-cli/src/mcp.rs` constructs the app core locally. CLI smoke tests negotiate stdio/HTTP, call tools, and enforce static tokens; the no-default run verifies local use and rejection of explicit OAuth-only options. |
| Device-global definitions; no activation through vault config or sync | `vulcan-daemon/src/registry.rs` stores named definitions alongside daemon configuration. `named_mcp_remote_cli_lifecycle_is_device_global_and_dry_run_safe` checks registry location, preview immutability, lifecycle output, and unchanged vault content. |
| Stable identities, registered vaults, safe URL/listener policy and collisions | `mcp_remote.rs` and registry validation bind immutable instance ULIDs, HTTPS resources, loopback listeners, and registered wiki IDs. Registry tests cover identity-preserving updates, conflicting bind/resource definitions, and refusal to unregister an exposed wiki. |
| Concurrent independent remotes and ownership | `two_named_hosted_http_listeners_bind_and_stop_independently` exercises actual listeners with separate metadata, audiences, sessions and ledgers. Runtime lock tests reject the same instance without blocking another. |
| Complete `remote init/list/show/set/run/remove` and `connections list/show/revoke` surfaces | `vulcan-cli/src/commands/mcp_remote.rs` exposes JSON and dry-run management. Lifecycle, multi-vault management, migration, consent and revocation regressions cover the commands, running-instance refusal, and independent grant removal. |
| Multi-vault consent and dispatch | `mcp_remote_runtime.rs` requires an explicit exposed vault when more than one is eligible. `named_consent_routes_each_grant_to_its_selected_vault` and the live foreground/resident test route each grant to its selected vault. Removed vaults fail authorization and revoke only affected grants/families. |
| Human authentication distinct from explicit consent | Shared `mcp_oauth_authorize`, `mcp_oauth_browser`, and `mcp_oauth_consent` preserve upstream state/PKCE, render the exact client/resource/vault/profile/packs/expiry, validate CSRF, and issue a code only after approval. Unit and live tests reject CSRF, replay and denial. The live test substitutes only upstream token exchange, not the MCP OAuth lifecycle. |
| Monotonic per-agent authority and server ceiling | `mcp_remote_runtime.rs` creates bounded grants over the existing `PermissionGrant`/guard/filter contracts. Tests reject profile expansion and ineligible packs, enforce current restrictions after queueing, and persist narrowing across restart so later profile widening cannot restore authority. |
| Scope/resource propagation and challenges | Shared policy, code, token, metadata and HTTP-auth modules validate exact resources and supported scopes. Token and live tests preserve claims/responses and compare protected-resource/issuer metadata and bearer/insufficient-scope challenges between hosts. |
| Short-lived access tokens, rotating refresh, expiry and revocation | `mcp_state.rs` stores hashed refresh secrets and locked durable grants/families. Rotation/replay tests revoke a reused family; runtime tests bind client/resource/scope. Live tests refresh across resident restart and reject access/refresh after revocation while another grant remains usable. |
| Exact session authority for POST, SSE and DELETE | `mcp_session.rs` captures instance/grant/client/subject/vault/audience/profile/packs/credential fingerprint. Cross-subject/grant/remote/token tests reject attachment to an existing session. Driver tests exercise authenticated POST and DELETE ownership; SSE tests retire revoked or expired authority. |
| Per-session catalogs, resources, notifications and cancellation | The app core owns schemas, packs, pagination, prompts, resources, completions and ephemeral large results. Live tests isolate equal result URIs between sessions, compare paginated catalogs, scope notifications, narrow tool catalogs, and cancel queued work without publishing its note. Session tests isolate active IDs/tokens and bound lagging subscribers. |
| One dispatcher and one HTTP/OAuth adapter across hosts | `vulcan-app::mcp_session_protocol` and `mcp_tool_exec` own protocol/tool dispatch without CLI types. `vulcan-daemon::mcp_http_driver`, `mcp_http_host`, `mcp_execution`, `mcp_transport`, and OAuth modules own transport/session/execution policy. CLI injection supplies only parsed configuration and trusted help/registry catalog data. Default and no-default transport tests preserve report/error shapes. |
| Resident supervision, scheduler and shutdown | `process.rs` supplies the same scheduler to the resident host and MCP ingress factories. `HostedMcpExecution` revalidates original authority after queueing and retains permits until workers finish. Live tests use the daemon supervisor; readiness/failure tests cancel and join listeners, while shutdown tests close sessions and SSE. |
| Mutation coordination and unknown-write recovery | Shared vault gates exclude cooperating processes; app note/task/template workflows stale-check writes and journal coordinated effects. Named mutations register in the durable hosted-operation ledger before worker launch. Timeout tests retain the operation ID and caller-bound status path, retire stale sessions, and distinguish never-dispatched from indeterminate writes. Ordinary-write recovery tests refuse partial reads and preserve repair evidence. Plugin/host-process effects are not claimed to be rollback-atomic. |
| Bounded HTTP and browser state | Codec/transport/session/browser tests cover body/header limits, duplicate security headers, framing rejection, total read deadline, write timeout, connection/session admission, idle expiry, queue limits, and bounded single-use login/consent/code state. |
| Confidential/public clients, DCR and advanced compatibility; current CIMD remains incomplete | Shared client/policy/token modules enforce exact redirect URIs, allowlisted hosts, declared Basic/post/none methods and public-client PKCE. Unit/live tests exercise confidential/public clients, legacy CIMD rejection cases and one-time code exchange. The current plural CIMD method contract is an open compatibility gap below. Direct OAuth/OIDC flags remain separate from the named-remote registry. |
| Durable, protected and secret-free state; migrations | `vulcan-secrets` and `mcp_credentials.rs` provide instance-bound references, version-2 reference-only clients, immutable binding receipts and explicit locked legacy migration. Tests cover dry-run immutability, partial-copy replay, lost/changed credentials, exact-key restoration and preserved grants. Reports/debug/error paths omit credential values. See [credential custody](mcp-credential-custody.md). |
| Help, configuration, permissions, proxy examples and bundled skills | CLI lifecycle/help/describe tests and installed-agent payload tests cover shipped command/workflow availability. `mcp-setup` documents consent, grant boundaries, proxy transport, revocation, operation-status recovery and exact-key custody recovery. The skill installs in `.agents/skills/mcp-setup/SKILL.md`; its existing Vulcan-specific frontmatter is checked by repository tests rather than the generic Codex skill validator. |

## Open gates and limits

- Current ChatGPT CIMD compatibility is incomplete. [Official OpenAI authentication guidance](https://developers.openai.com/plugins/build/auth)
  advertises a plural method list including `none` while the legacy singular preference may be
  `private_key_jwt`. Vulcan currently reads only the singular field and rejects that otherwise
  usable public-client document. Add strict plural-field parsing and public-method intersection,
  retaining legacy fallback only when the plural field is absent; do not advertise JWT support
  without implementing assertion verification. This is a concrete implementation gap, not a claim
  that live ChatGPT was tested.
- Roadmap 10.10's resident-hosting checkbox explicitly depends on 10.7.1–10.7.6. The MCP dispatch,
  HTTP-hosting and stdio items are verified and checked. Section 10.7.6 still includes adaptation
  of conflict/semantic automation and new Phase 11 supervised auto-commit plus its integration tests.
  Those items are not marked complete. The accepted [hosting contract](unified-hosting.md) explicitly
  lists new Phase 11 behavior as a non-goal. This scope discrepancy needs an explicit decision;
  this audit does not silently remove the dependency or redefine the goal.
- Native Windows execution of the current commits is unverified. Linux tests and earlier protected-store
  cross-target checks are not substitutes. Full daemon cross-compilation is unavailable without the
  required Windows C compiler. No changes have been pushed or remote CI dispatched by this work.
- Live ChatGPT connection acceptance is not proved by fake-client conformance. The hosted-client
  protocol contract is covered locally, but a real connection requires the user's public ingress,
  IndieAuth identity and client account. Reverse proxies/tunnels remain transport, not authority.

The goal remains open until its remaining requirements are verified and the dependency scope is resolved.
