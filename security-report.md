# Vulcan Security Report

Generated from the sealed Codex Security deep scan artifacts for this repository.

## Scan Provenance

- **Scan ID:** `ecbd2fec58c4d761c1031719292cf6468bcfae5c_20260710T163912Z`
- **Target revision:** `ecbd2fec58c4d761c1031719292cf6468bcfae5c`
- **Started:** `2026-07-10T16:39:12Z`
- **Completed:** `2026-07-10T17:27:30Z`
- **Coverage:** `complete`
- **Reviewed scope:** `.`
- **Findings:** `30` total, `14` high, `16` medium
- **External follow-ups:** `12` additional dependency, secret-scan, SAST, unsafe-code, and supply-chain policy items
- **Source report:** `/tmp/codex-security-scans/vulcan/ecbd2fec58c4d761c1031719292cf6468bcfae5c_20260710T163912Z/report.md`
- **Findings JSON:** `/tmp/codex-security-scans/vulcan/ecbd2fec58c4d761c1031719292cf6468bcfae5c_20260710T163912Z/findings.json`
- **SARIF export:** `/tmp/codex-security-scans/vulcan/ecbd2fec58c4d761c1031719292cf6468bcfae5c_20260710T163912Z/exports/results.sarif`

## Scan Limitations

- Network access to crates.io was denied during dependency advisory checks to avoid disclosing private dependency metadata.
- cargo-audit, cargo-deny, semgrep, trufflehog, and gitleaks were not installed in the environment.
- DoS candidates were validated by static source trace rather than intentionally exhausting local resources.
- Follow-up required: Dependency advisory tooling was unavailable offline; cargo-audit/cargo-deny/semgrep/trufflehog/gitleaks were not installed and crates.io access was denied by policy.

## Implementation Handoff

This report is intended to be self-contained for an implementation agent. The agent should use this report plus the repository as the working context. The `/tmp` artifact paths are useful evidence if still present, but the findings, affected locations, fix directions, and checklists below should be sufficient even if those temporary files are gone.

### Start Here

Fix shared security primitives first, then apply them consistently to the individual findings. Do not start by patching each call site independently; many findings share the same broken boundaries.

1. **Path containment and no-follow filesystem access:** build a central helper for vault-contained reads/writes that rejects absolute paths, parent traversal, symlink escapes, and not-yet-existing outside-vault paths. Do not rely only on `canonicalize()` for write targets because the final path may not exist yet.
2. **Permission-filter threading:** make the selected `PermissionFilter` flow through every read/export/report/MCP/plugin/search/vector/publication path. Denied content must be filtered before summarization, clustering, export manifests, static indexes, attachments, hover data, and saved reports.
3. **Sandbox and trust boundaries:** make JS sandbox tiers, host command execution, network access, executable config, aliases, provider URLs, and secret environment-variable names fail closed unless they come from trusted local configuration or an explicit high-trust mode.
4. **Publication and rendering safety:** sanitize rendered HTML by default, validate URL schemes, and ensure export/site generation uses the same permission and path checks as normal vault reads.
5. **Network and HTTP hardening:** re-check allowlists after redirects, cap request/response sizes, authenticate loopback data APIs by default, and validate OAuth redirect URIs against registered values.
6. **Dependency and supply-chain cleanup:** apply the `DEP-*`, `SEC-*`, `SAST-*`, `UNSAFE-*`, and `SC-*` follow-ups after or alongside the source-level fixes.

### Suggested Work Packages

- [ ] **Shared path and secure write helper:** cover `H-03`, `H-08`, `H-11`, `H-12`, `H-13`, `M-01`, `M-07`, `M-10`, `M-13`.
- [ ] **Permission-filter propagation:** cover `H-06`, `H-10`, `M-05`, `M-06`, `M-08`, `M-11`, `M-12`.
- [ ] **Sandbox/config trust gate:** cover `H-02`, `H-09`, `H-14` and the executable parts of `SAST-01`.
- [ ] **Rendering/export/publication hardening:** cover `H-05`, `H-06`, `H-07`, `M-14`.
- [ ] **HTTP/OAuth/network hardening:** cover `H-01`, `H-04`, `M-02`, `M-03`, `M-04`.
- [ ] **Resource-budget controls:** cover `M-15`, `M-16` and the unbounded-output parts of extraction/parser/query evaluation.
- [ ] **Dependency and tooling follow-up:** cover `DEP-01` through `DEP-05`, `SEC-01`, `SEC-02`, `SAST-01`, `SAST-02`, `UNSAFE-01`, `SC-01`, `SC-02`.

### Suggested Commit Boundaries

- [ ] Commit shared path containment and no-follow filesystem primitives separately from call-site migrations.
- [ ] Commit permission-filter plumbing separately from behavior changes that depend on it.
- [ ] Commit JS sandbox/config trust changes as one coherent security-boundary change.
- [ ] Commit renderer/export/publication changes together only when they share tests and behavior.
- [ ] Commit HTTP/OAuth/network hardening separately from vault filesystem work.
- [ ] Commit dependency updates separately from source changes.
- [ ] Commit `deny.toml`, secret-scan allowlists, and `cargo-vet` setup separately from vulnerability fixes.
- [ ] Avoid one large "fix security report" commit; use self-contained commits that pass relevant checks.

### Definition of Done

Each finding should only be checked off when all of these are true:

- [ ] A regression test or fixture demonstrates the vulnerable behavior would fail before the fix.
- [ ] The implementation uses shared helpers where appropriate instead of repeating boundary checks ad hoc.
- [ ] The fix does not broaden trust, sandbox, filesystem, network, or publication permissions to preserve compatibility.
- [ ] The narrow relevant test target passes.
- [ ] `cargo fmt --all` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace` passes before final closure.
- [ ] Relevant external scanners are re-run when applicable: `cargo audit`, `cargo deny check advisories`, `osv-scanner scan -r .`, `semgrep`, `gitleaks`, or `trufflehog`.
- [ ] Any accepted residual risk is documented next to the affected finding and in the commit or PR notes.

### Implementation Guardrails

- [ ] Do not close a finding by adding a broad scanner suppression, broad allowlist, or blanket permission bypass.
- [ ] Do not use only `Path::canonicalize()` as the security boundary for writes, generated files, or paths that may not exist yet.
- [ ] Do not follow symlinks for security-sensitive reads or writes unless the trust model explicitly allows it and tests cover it.
- [ ] Do not put reusable business logic only in `vulcan-cli`; prefer `vulcan-core` for reusable semantics and `vulcan-app` for reusable synchronous workflows.
- [ ] Do not make JS sandbox tier `none`, network access, filesystem access, or host command execution the default.
- [ ] Do not let vault-shared config select executable commands, aliases, provider URLs, or secret environment-variable names without a trust gate.
- [ ] Do not filter denied content after summarization, clustering, rendering, static search export, or attachment collection; filter before derived output is created.
- [ ] Do not treat a dependency advisory as fixed until both `Cargo.lock` and `fuzz/Cargo.lock` have been checked where applicable.

### Known Ambiguity

- [ ] `SEC-01` may be a public Discord client ID rather than a secret. Verify ownership and intent before deciding whether to replace, rotate, or allowlist it.
- [ ] `SEC-02` appears to be test data using `example.test`; make it scanner-safe or narrowly allowlist it after confirming it is not live.
- [ ] `DEP-04` (`quinn-proto`) and `DEP-05` (`anyhow`) appear in lockfiles but were not reachable in the default workspace graph during `cargo tree -i`; verify target/feature reachability before prioritizing.
- [ ] `cargo-deny` full-check license failures are not yet license findings because the repo has no reviewed `deny.toml`.
- [ ] `cargo-geiger` dependency counts are advisory because it reported a parser warning in `signal-hook-registry`; first-party unsafe counts should still be reviewed.
- [ ] The original deep scan intentionally avoided destructive DoS reproduction, so resource-exhaustion fixes need bounded stress tests rather than unbounded local exhaustion.

## External Tool Validation

Additional tooling was installed and run after the sealed Codex Security scan. Raw outputs are saved under `/tmp/vulcan-security-tools/`.

| Tool | Version | Result |
| --- | --- | --- |
| `cargo audit` | `cargo-audit-audit 0.22.2` | Found 4 vulnerability advisories and 1 unsoundness warning in `Cargo.lock`. |
| `cargo deny` | `cargo-deny 0.20.2` | Advisory-only check confirmed 3 RustSec advisories; full check also failed license policy because no `deny.toml` exists. |
| `osv-scanner` | `2.4.0` | Found 9 vulnerability groups across `Cargo.lock` and `fuzz/Cargo.lock`, matching RustSec/OSV data. |
| `semgrep` | `1.169.0` | Ran `p/rust`; found 8 INFO-level audit findings and 2 rule timeouts on `vulcan-core/src/config/mod.rs`. |
| `gitleaks` | `8.30.1` | Found 1 historical and 1 current source candidate for a Discord client ID; build-artifact hits were ignored. |
| `trufflehog` | `3.95.9` | No-verification source scan found 1 unverified URI credential test fixture candidate. |
| `cargo-geiger` | `0.13.0` | Found first-party unsafe expressions in `vulcan-core` and `vulcan-embed`; dependency parser warning on `signal-hook-registry`. |
| `cargo vet` | `0.10.2` | Could not check because the repo has no `supply-chain/` vet store. |

### Dependency Advisory Findings

#### DEP-01. `ammonia 4.1.2` mXSS advisory

- **Advisory:** `RUSTSEC-2026-0193`, `GHSA-9jh8-v38h-cvhr`
- **Detected by:** `cargo audit`, `cargo deny`, `osv-scanner`
- **Affected paths:** `Cargo.toml:13`, `vulcan-core/Cargo.toml:17`, `Cargo.lock`
- **Impact:** This reinforces `H-05`; vulnerable sanitizer versions can allow browser JavaScript execution when unsafe MathML/tag combinations are enabled.
- **Fix direction:** Upgrade `ammonia` to `>=4.1.3` and keep raw HTML sanitization fail-closed by default.

**Action checklist:**

- [ ] Update the workspace `ammonia` dependency to `>=4.1.3`.
- [ ] Run `cargo update -p ammonia` or an equivalent lockfile update.
- [ ] Add an HTML sanitizer regression test covering the MathML `annotation-xml` gadget class.
- [ ] Re-run `cargo audit`, `cargo deny check advisories`, and `osv-scanner scan -r .`.

#### DEP-02. `rsa 0.9.10` Marvin timing-side-channel advisory

- **Advisory:** `RUSTSEC-2023-0071`, `CVE-2023-49092`
- **Detected by:** `cargo audit`, `cargo deny`, `osv-scanner`
- **Affected paths:** `Cargo.toml:34`, `vulcan-core/Cargo.toml:23`, `Cargo.lock`
- **Dependency path:** `jsonwebtoken 10.3.0 -> rsa 0.9.10`
- **Impact:** Network-observable RSA private-key operations can leak timing information. This is especially relevant to OAuth/JWT work and should be considered alongside `H-01`.
- **Fix direction:** Avoid RSA signing/decryption paths, reject `RS*`/`PS*` JWT algorithms unless a safe crypto backend is available, or replace the dependency stack.

**Action checklist:**

- [ ] Audit all JWT encode/decode paths and explicitly restrict accepted algorithms.
- [ ] Add tests that reject unexpected RSA JWT algorithms in local OAuth/MCP token handling.
- [ ] Decide whether to replace `jsonwebtoken`/`rsa` or document that RSA functionality is unreachable and forbidden.
- [ ] Re-run RustSec/OSV scanners and record any accepted residual risk if no patched `rsa` version exists.

#### DEP-03. `crossbeam-epoch 0.9.18` invalid pointer dereference advisory

- **Advisory:** `RUSTSEC-2026-0204`
- **Detected by:** `cargo audit`, `cargo deny`, `osv-scanner`
- **Affected paths:** `Cargo.lock`, via `ignore -> crossbeam-deque` and `rayon -> rayon-core`
- **Impact:** Formatting invalid `Atomic`/`Shared` pointers with affected versions can dereference invalid pointers.
- **Fix direction:** Upgrade to `crossbeam-epoch >=0.9.20` through transitive dependency updates.

**Action checklist:**

- [ ] Run `cargo update -p crossbeam-epoch`.
- [ ] Confirm `ignore`, `rayon`, and related transitive crates still resolve cleanly.
- [ ] Re-run `cargo test --workspace`.
- [ ] Re-run `cargo audit`, `cargo deny check advisories`, and `osv-scanner scan -r .`.

#### DEP-04. `quinn-proto 0.11.14` remote memory exhaustion advisory

- **Advisory:** `RUSTSEC-2026-0185`, `GHSA-4w2j-m93h-cj5j`
- **Detected by:** `cargo audit`, `osv-scanner`
- **Affected paths:** `Cargo.lock`, `fuzz/Cargo.lock`
- **Reachability note:** `cargo tree -i quinn-proto` did not show reachability in the default workspace graph; treat this as a lockfile/feature-target verification item until proven reachable.
- **Fix direction:** Upgrade to `quinn-proto >=0.11.15` or update/remove the transitive feature path that keeps it in lockfiles.

**Action checklist:**

- [ ] Identify the feature or target configuration that keeps `quinn-proto` in `Cargo.lock` and `fuzz/Cargo.lock`.
- [ ] Update the transitive dependency chain so `quinn-proto >=0.11.15` is selected, or remove stale lockfile entries if unreachable.
- [ ] Re-run `cargo tree --target all -i quinn-proto` and document reachability.
- [ ] Re-run `cargo audit` and `osv-scanner scan -r .`.

#### DEP-05. `anyhow 1.0.102` unsoundness warning

- **Advisory:** `RUSTSEC-2026-0190`
- **Detected by:** `cargo audit` warning and `osv-scanner`
- **Affected paths:** `Cargo.lock`
- **Reachability note:** `cargo tree -i anyhow` did not show default workspace reachability; verify target/feature reachability before prioritizing.
- **Fix direction:** Upgrade to `anyhow >=1.0.103` or remove stale lockfile entries.

**Action checklist:**

- [ ] Run `cargo update -p anyhow`.
- [ ] Re-run `cargo tree --target all -i anyhow` and document whether it is reachable.
- [ ] Re-run `cargo audit` and `osv-scanner scan -r .`.

### Secret Scan Findings

#### SEC-01. Discord client ID candidate in roadmap

- **Detected by:** `gitleaks git`, `gitleaks dir`
- **Affected paths:** current `docs/ROADMAP.md:4621`; historical commit `003b8e504620de5a75b14b53b5556a122a5909c3` at `docs/ROADMAP.md:4184`
- **Assessment:** Discord client IDs are often public identifiers rather than secrets, but this should be verified and either replaced with a placeholder or explicitly allowlisted.

**Action checklist:**

- [ ] Determine whether the Discord client ID is a real application identifier or documentation placeholder.
- [ ] If real and unnecessary, replace it with a placeholder and rotate/recreate the Discord application if appropriate.
- [ ] If intentionally public, add a narrow `gitleaks` allowlist entry with a comment explaining why it is safe.
- [ ] Re-run `gitleaks git --redact .` and `gitleaks dir --redact .`.

#### SEC-02. Test URI credential candidate in vector command tests

- **Detected by:** `trufflehog git` and source-only `trufflehog filesystem`
- **Affected paths:** `vulcan-cli/src/commands/vectors.rs:1049`, `vulcan-cli/src/commands/vectors.rs:1053`
- **Assessment:** The candidate is an unverified `example.test` URI fixture containing `user:secret`. It appears to be test data, not a live credential, but should be made obviously non-secret or allowlisted.

**Action checklist:**

- [ ] Replace `user:secret` fixture text with clearly fake placeholders that secret scanners do not flag, or add a narrow documented allowlist.
- [ ] Re-run `trufflehog git file://"$PWD" --no-verification --json`.
- [ ] Re-run the source-only TruffleHog filesystem scan excluding `.git`, `target/`, and vendored references.

### Static Analysis and Unsafe-Code Follow-Ups

#### SAST-01. Semgrep Rust audit findings

- **Detected by:** `semgrep scan --config=p/rust`
- **Affected paths:** `vulcan-cli/src/commands/completions.rs:236`, `vulcan-cli/src/commands/edit.rs:214`, `vulcan-cli/src/lib.rs:2104`, `vulcan-cli/src/main.rs:77`, `vulcan-core/src/expression/value.rs:134`, `vulcan-core/src/expression/value.rs:161`, `vulcan-core/src/git.rs:504`, `vulcan-embed/src/sqlite_vec.rs:14`
- **Assessment:** All Semgrep hits were INFO-level audit findings. They do not replace the 30 source-traced findings, but they identify places to confirm `current_exe`, `args_os`, `temp_dir`, and `unsafe` are not part of security decisions without guardrails.

**Action checklist:**

- [ ] Audit `current_exe` and `args_os` usage and confirm they are not trusted for security decisions.
- [ ] Audit `temp_dir` usage and ensure all temporary files/directories are created with unpredictable names and secure open/create semantics.
- [ ] Document safety invariants for the unsafe blocks in `vulcan-core/src/expression/value.rs` and `vulcan-embed/src/sqlite_vec.rs`.
- [ ] Consider `#![forbid(unsafe_code)]` for crates/modules that do not require unsafe code.
- [ ] Re-run Semgrep with a larger timeout for `vulcan-core/src/config/mod.rs`.

#### SAST-02. Semgrep timeouts on config module

- **Detected by:** `semgrep`
- **Affected path:** `vulcan-core/src/config/mod.rs`
- **Assessment:** Two Rust rules timed out on this file: `rust.lang.security.args.args` and `rust.lang.security.unsafe-usage.unsafe-usage`.

**Action checklist:**

- [ ] Re-run Semgrep with a higher per-rule timeout against `vulcan-core/src/config/mod.rs`.
- [ ] Add targeted local Semgrep rules for config trust boundaries if registry rules remain too broad.

#### UNSAFE-01. `cargo-geiger` unsafe inventory

- **Detected by:** `cargo-geiger`
- **First-party counts:** `vulcan-core` has 14 used unsafe expressions; `vulcan-embed` has 3 used unsafe expressions; `vulcan-app` and `vulcan-cli` reported 0 used unsafe expressions in their own crates.
- **Limitation:** `cargo-geiger` reported a parser warning for `signal-hook-registry`, so dependency unsafe counts should be treated as advisory rather than complete.

**Action checklist:**

- [ ] Add or tighten comments documenting every first-party unsafe invariant.
- [ ] Add regression tests around unsafe conversion and SQLite extension-loading boundaries.
- [ ] Decide whether `vulcan-app` and `vulcan-cli` should explicitly forbid unsafe code.

### Supply-Chain Policy Follow-Ups

#### SC-01. Missing `cargo-deny` policy

- **Detected by:** `cargo deny check advisories bans licenses sources`
- **Assessment:** Advisory-only checking worked, but the full check produced large license failures because the repo has no `deny.toml` policy. That is policy noise, not a license conclusion.

**Action checklist:**

- [ ] Add a reviewed `deny.toml` with allowed licenses, source policy, duplicate-crate policy, and advisory policy.
- [ ] Run `cargo deny check advisories bans licenses sources` in CI.

#### SC-02. Missing `cargo-vet` store

- **Detected by:** `cargo vet check`
- **Assessment:** `cargo-vet` is installed, but the repo has no `supply-chain/` store, so no vet policy can be checked.

**Action checklist:**

- [ ] Decide whether Vulcan should adopt `cargo-vet`.
- [ ] If yes, run `cargo vet init`, review generated policy, and commit the `supply-chain/` store.
- [ ] Add `cargo vet check` to CI once initialized.

## Remediation Program

- [ ] Open one tracking issue per finding with the finding ID, severity, owner, and target release.
- [ ] Fix all high-severity findings before enabling daemon, MCP, publishing, assistant, or JS sandbox features for untrusted vaults/users.
- [ ] Add regression tests before or with each fix, using hostile vault fixtures for path, symlink, config, and permission-profile cases.
- [ ] Run `cargo fmt --all`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace`.
- [ ] Re-run the deep security scan and compare the new SARIF/findings against this report.
- [ ] Run dependency and secret scanning with network/tooling available: `cargo audit`, `cargo deny`, `semgrep`, `trufflehog`, and `gitleaks`.

## Cross-Cutting Hardening Work

These items should be implemented once and reused across individual fixes to avoid call-site drift.

- [ ] Create a central vault path containment helper that rejects absolute paths, parent traversal, and symlink escapes using no-follow metadata where mutation is possible.
- [ ] Thread the selected `PermissionFilter` through every read, export, report, MCP, plugin, DataviewJS, search, vector, and publication path.
- [ ] Make sandbox permissions fail closed: missing profiles deny host I/O, network, process execution, and filesystem access.
- [ ] Separate trusted local configuration from vault-shared configuration for executable commands, aliases, provider URLs, and secret environment-variable names.
- [ ] Add output, request-body, recursion-depth, node-count, and timeout budgets to parser, expression, extraction, MCP, and HTTP surfaces.
- [ ] Add a security fixture suite with malicious symlinks, outside-vault paths, hostile frontmatter, raw HTML/JavaScript, redirecting HTTP endpoints, and restrictive permission profiles.

## Finding Index

| ID | Severity | Title | Primary area |
| --- | --- | --- | --- |
| H-01 | HIGH | Local OAuth client secret also signs access tokens | `vulcan-core/src/oauth.rs` |
| H-02 | HIGH | Strict JavaScript sandboxes can still execute host commands | `vulcan-core/src/dataview_js.rs` |
| H-03 | HIGH | DataviewJS file APIs bypass read profiles and follow symlinks | `vulcan-core/src/dataview_js.rs` |
| H-04 | HIGH | Web fetch allowlists are checked before redirects | `vulcan-core/src/permissions.rs` |
| H-05 | HIGH | Markdown HTML rendering allows raw script and dangerous URL schemes by default | `vulcan-core/src/html.rs` |
| H-06 | HIGH | Exports and site builds can publish files outside the active read filter | `vulcan-app/src/export.rs` |
| H-07 | HIGH | Vault-controlled export and site output paths can write or delete outside the vault | `vulcan-app/src/export.rs` |
| H-08 | HIGH | Assistant prompt and skill roots can read host files through config or symlinks | `vulcan-core/src/assistant.rs` |
| H-09 | HIGH | Vault configuration can exfiltrate env secrets and note text through HTTP backends | `vulcan-core/src/web.rs` |
| H-10 | HIGH | Saved reports and automation bypass selected read permission profiles | `vulcan-cli/src/lib.rs` |
| H-11 | HIGH | Bulk mutation APIs accept `../` and absolute paths before writing | `vulcan-cli/src/lib.rs` |
| H-12 | HIGH | Vault note and state-file writes follow symlinks outside the vault | `vulcan-app/src/notes.rs` |
| H-13 | HIGH | Templater include resolves absolute and parent paths outside the vault | `vulcan-app/src/templates.rs` |
| H-14 | HIGH | Shared vault config can trigger command execution or command rewriting without a trust gate | `vulcan-core/src/scan.rs` |
| M-01 | MEDIUM | `web fetch --save` writes to arbitrary filesystem paths | `vulcan-cli/src/commands/runtime.rs` |
| M-02 | MEDIUM | MCP HTTP allocates unbounded request bodies before authentication | `vulcan-cli/src/mcp.rs` |
| M-03 | MEDIUM | MCP OAuth redirects accept arbitrary raw redirect URIs | `vulcan-cli/src/mcp.rs` |
| M-04 | MEDIUM | Loopback serve API exposes vault data without default auth or browser origin checks | `vulcan-cli/src/serve.rs` |
| M-05 | MEDIUM | Filtered vector clustering summarizes denied chunks before filtering | `vulcan-core/src/vector.rs` |
| M-06 | MEDIUM | Dataview inline and expression evaluation use unrestricted note lookup | `vulcan-core/src/properties.rs` |
| M-07 | MEDIUM | Bases, Kanban, and note move rewrites follow symlinked vault files | `vulcan-core/src/bases.rs` |
| M-08 | MEDIUM | Plugin command handlers skip selected permission profiles | `vulcan-cli/src/commands/kanban.rs` |
| M-09 | MEDIUM | Cache-controlled table names are interpolated into `execute_batch` SQL | `vulcan-core/src/cache/schema.rs` |
| M-10 | MEDIUM | Git commands can access paths outside a vault nested in a larger worktree | `vulcan-core/src/git.rs` |
| M-11 | MEDIUM | Static search export emits all indexed content without a permission filter | `vulcan-core/src/search.rs` |
| M-12 | MEDIUM | MCP task and suggestion read tools bypass read filters | `vulcan-cli/src/mcp.rs` |
| M-13 | MEDIUM | Site preview server serves symlinked files outside the output directory | `vulcan-cli/src/site_server.rs` |
| M-14 | MEDIUM | Frontmatter redaction can miss valid YAML keys during publication transforms | `vulcan-core/src/content_transforms.rs` |
| M-15 | MEDIUM | Attachment extraction buffers unlimited command output before applying caps | `vulcan-core/src/extraction.rs` |
| M-16 | MEDIUM | Parser, query, and expression engines contain unbounded recursion and allocation paths | `vulcan-core/src/dql/token.rs` |

## High Severity Findings

### H-01. Local OAuth client secret also signs access tokens

- **Finding ID:** `csf_b3a0aadce4c1574abd787c6f`
- **Rule:** `authentication.token-forgery`
- **Severity:** `high` (Token forgery directly affects identity and permission-profile assignment on an auth boundary.)
- **Confidence:** `high` (Source review directly traces the shared secret from token issuance and validation to trusted identity claims.)
- **Taxonomy:** Authentication bypass; `CWE-347`, `CWE-287`

**Summary:** The local OAuth issuer validates HS256 access tokens with the same secret that clients use to authenticate. Any holder of that client secret can mint tokens containing arbitrary `sub` and `permission_profile` claims and bypass the authorization-code approval flow.

**Root cause:** The invariant should be that client authentication secrets cannot sign bearer tokens. The implementation reuses one symmetric secret for both client authentication and access-token trust.

**Reachability:** A local or MCP OAuth client that knows the secret can trigger this when local issuer support is enabled.

**Data flow:** Client secret -> forged HS256 JWT -> `validate_access_token` -> trusted subject/profile.

**Fix direction:** Use a server-only signing key for local access tokens, store it separately from client secrets, reject externally supplied symmetric tokens, and bind allowed permission profiles server-side.

**Scanner validation:** Source review directly traces the shared secret from token issuance and validation to trusted identity claims.

**Affected locations:**

- `vulcan-core/src/oauth.rs:235` (root_control)
- `vulcan-core/src/oauth.rs:279` (sink)
- `vulcan-core/src/oauth.rs:287` (entrypoint)
- `vulcan-core/src/oauth.rs:241` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-01`.
- [ ] Add or update a regression test: Unit test that a token signed with the client secret is rejected.
- [ ] Add or update a regression test: Unit test that `permission_profile` claims are selected from server-side authorization state, not caller-supplied JWTs.
- [ ] Implement the remediation: Use a server-only signing key for local access tokens, store it separately from client secrets, reject externally supplied symmetric tokens, and bind allowed permission profiles server-side.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-02. Strict JavaScript sandboxes can still execute host commands

- **Finding ID:** `csf_837194ce22e0cfaad482ca2c`
- **Rule:** `sandbox-escape.host-process`
- **Severity:** `high` (This crosses the primary sandbox boundary and reaches command execution.)
- **Confidence:** `high` (Static trace plus local proof from discovery showed a trusted strict skill command invoking `host.exec` and returning command output.)
- **Taxonomy:** Sandbox escape / command execution; `CWE-78`, `CWE-94`

**Summary:** `host.exec` and `host.shell` are installed in the JS runtime even for strict-mode execution, and the execution guards check only optional permission profiles. When no active profile is supplied, trusted skill commands and DataviewJS paths can spawn host processes despite the documented strict sandbox semantics.

**Root cause:** The invariant should be that `strict` means pure computation and no I/O. Execution authority is instead tied to an optional permission profile, so the absence of a profile becomes fail-open.

**Reachability:** A malicious vault or trusted skill can trigger this when a user runs JS/skill tooling without an explicit restrictive profile.

**Data flow:** Vault script or trusted skill command -> JS `host.exec` -> optional permission guard -> `ProcessCommand` spawn.

**Fix direction:** Make sandbox tier checks fail-closed inside `ensure_execute_access` and `ensure_shell_access`; default missing profiles to deny host I/O; require explicit `none`/execute permission for host commands.

**Scanner validation:** Static trace plus local proof from discovery showed a trusted strict skill command invoking `host.exec` and returning command output.

**Affected locations:**

- `vulcan-core/src/dataview_js.rs:2772` (entrypoint)
- `vulcan-core/src/dataview_js.rs:5049` (root_control)
- `vulcan-core/src/dataview_js.rs:5161` (sink)
- `vulcan-app/src/tools/skill_commands.rs:588` (entrypoint)

**Action checklist:**

- [ ] Assign an owner and target release for `H-02`.
- [x] Add or update a regression test: Regression test that `host.exec` fails in strict mode with no profile.
- [x] Add or update a regression test: Regression test that trusted skill commands without an explicit profile cannot call host execution APIs.
- [x] Implement the remediation: Make sandbox tier checks fail-closed inside `ensure_execute_access` and `ensure_shell_access`; default missing profiles to deny host I/O; require explicit `none`/execute permission for host commands.
- [x] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [x] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [x] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [x] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-03. DataviewJS file APIs bypass read profiles and follow symlinks

- **Finding ID:** `csf_d856fb5756974c161233b2d5`
- **Rule:** `authorization-bypass.js-file-io`
- **Severity:** `high` (The issue exposes file contents across a documented sandbox/profile boundary and can cross the vault filesystem boundary.)
- **Confidence:** `high` (Static trace found no `ensure_fs_access` or read-profile check before the file reads; symlink-following sinks are ordinary `fs` operations.)
- **Taxonomy:** Authorization bypass / arbitrary file read-write; `CWE-22`, `CWE-862`, `CWE-200`

**Summary:** `dv.io.load`, CSV/view helpers, and related note-read callbacks normalize only lexical vault paths and then read or write through regular filesystem APIs. They do not apply read permission filters and they follow symlinks that point outside the vault.

**Root cause:** The invariant should be that JS file access is constrained by both sandbox tier and selected permission profile. The implementation treats a normalized vault-relative string as sufficient authorization.

**Reachability:** A vault note containing DataviewJS can reach this when a user renders or evaluates it under a restrictive profile.

**Data flow:** JS path -> lexical normalization -> `vault_root.join` -> filesystem read/write.

**Fix direction:** Route all JS file IO through one canonical no-follow containment helper and call the selected read/write permission guard before the filesystem operation.

**Scanner validation:** Static trace found no `ensure_fs_access` or read-profile check before the file reads; symlink-following sinks are ordinary `fs` operations.

**Affected locations:**

- `vulcan-core/src/dataview_js.rs:1876` (entrypoint)
- `vulcan-core/src/dataview_js.rs:4036` (entrypoint)
- `vulcan-core/src/dataview_js.rs:5959` (root_control)
- `vulcan-core/src/dataview_js.rs:6007` (root_control)
- `vulcan-core/src/dataview_js.rs:4697` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-03`.
- [ ] Add or update a regression test: JS test that `dv.io.load` cannot read a denied path.
- [ ] Add or update a regression test: JS test that a symlink under the vault pointing outside is rejected.
- [ ] Implement the remediation: Route all JS file IO through one canonical no-follow containment helper and call the selected read/write permission guard before the filesystem operation.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-04. Web fetch allowlists are checked before redirects

- **Finding ID:** `csf_9999e20b9470c026a15be48b`
- **Rule:** `ssrf.redirect-allowlist`
- **Severity:** `high` (This bypasses a core network permission boundary and can reach local/LAN services.)
- **Confidence:** `high` (A local proof used an allowed localhost redirector to fetch a denied 127.0.0.1 service and returned the denied secret body.)
- **Taxonomy:** SSRF / network allowlist bypass; `CWE-918`

**Summary:** CLI, JS, and MCP web fetch paths check the originally requested URL against the network permission profile, then use a default `reqwest` client that follows redirects. A permitted host can redirect to a denied loopback, LAN, or metadata endpoint and return its response.

**Root cause:** The invariant should be that every final network destination is authorized. The implementation authorizes only the first URL and follows redirects without rechecking the redirected host.

**Reachability:** Any caller with `web.fetch` or net-sandbox access can use an allowed redirecting host.

**Data flow:** Allowed URL -> HTTP redirect -> default client follows to denied URL -> response body returned.

**Fix direction:** Disable automatic redirects or install a redirect policy that re-runs `check_network` against each redirected URL before following it; add timeout and response-size limits.

**Scanner validation:** A local proof used an allowed localhost redirector to fetch a denied 127.0.0.1 service and returned the denied secret body.

**Affected locations:**

- `vulcan-core/src/permissions.rs:238` (root_control)
- `vulcan-core/src/web.rs:365` (sink)
- `vulcan-core/src/web.rs:466` (root_control)
- `vulcan-cli/src/commands/runtime.rs:398` (entrypoint)

**Action checklist:**

- [ ] Assign an owner and target release for `H-04`.
- [ ] Add or update a regression test: Integration test with allowed redirector to denied host must fail.
- [ ] Add or update a regression test: Unit test that JS and MCP fetch paths share the redirect policy.
- [ ] Implement the remediation: Disable automatic redirects or install a redirect policy that re-runs `check_network` against each redirected URL before following it; add timeout and response-size limits.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-05. Markdown HTML rendering allows raw script and dangerous URL schemes by default

- **Finding ID:** `csf_2b3537798dca5e0f7c51e54e`
- **Rule:** `xss.markdown-html-render`
- **Severity:** `high` (Stored XSS on a product web surface can compromise viewer data/actions on that origin.)
- **Confidence:** `high` (Source trace plus existing tests show `<script>` is preserved without diagnostics; URL-scheme filtering has no denylist for active schemes.)
- **Taxonomy:** Stored XSS / active content injection; `CWE-79`, `CWE-80`

**Summary:** The HTML renderer defaults to raw HTML passthrough and preserves unrecognized URL schemes such as `javascript:` and active `data:` URLs in links/images. Vault Markdown rendered into the web/wiki/static-site surface can therefore execute attacker-supplied browser script.

**Root cause:** The invariant should be that untrusted vault Markdown is sanitized before browser rendering. The default renderer preserves active HTML and dangerous URLs.

**Reachability:** Any viewer of rendered notes or static site output can receive the active content.

**Data flow:** Vault Markdown -> pulldown HTML events/link URLs -> rendered HTML output.

**Fix direction:** Make sanitized rendering the default, strip or escape raw HTML unless explicitly trusted, and allowlist safe URL schemes for `href`/`src`.

**Scanner validation:** Source trace plus existing tests show `<script>` is preserved without diagnostics; URL-scheme filtering has no denylist for active schemes.

**Affected locations:**

- `vulcan-core/src/html.rs:37` (root_control)
- `vulcan-core/src/html.rs:63` (root_control)
- `vulcan-core/src/html.rs:1370` (sink)
- `vulcan-core/src/html.rs:1289` (sink)
- `vulcan-core/src/html.rs:1727` (root_control)

**Action checklist:**

- [ ] Assign an owner and target release for `H-05`.
- [ ] Add or update a regression test: Renderer test that `<script>` is escaped/removed by default.
- [ ] Add or update a regression test: Renderer test that `javascript:` and unsafe `data:` URLs are rejected.
- [ ] Implement the remediation: Make sanitized rendering the default, strip or escape raw HTML unless explicitly trusted, and allowlist safe URL schemes for `href`/`src`.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-06. Exports and site builds can publish files outside the active read filter

- **Finding ID:** `csf_04fb8371e01d0ba7b1856dda`
- **Rule:** `information-disclosure.export-permission-filter`
- **Severity:** `high` (This is a direct confidentiality break at the publication boundary.)
- **Confidence:** `high` (A local ZIP proof exported a public note under a public-only profile and included `Private/secret.txt` in the archive.)
- **Taxonomy:** Information disclosure / publication leak; `CWE-200`, `CWE-862`

**Summary:** Export preparation applies read filters to selected notes, but attachment collection and site extra asset handling read linked or configured files without the same authorization boundary. A public note can embed a denied attachment, and site profiles can copy absolute or outside-vault assets into the generated output.

**Root cause:** The invariant should be that publication/export output contains only data authorized by the active read filter. Attachments and extra assets bypass that filter.

**Reachability:** A malicious vault note or profile can cause a user to export/build a public artifact that includes private content.

**Data flow:** Allowed note/profile config -> unfiltered attachment/asset collection -> archive/site output.

**Fix direction:** Apply the same read filter to every attachment, embed, asset, hover/search artifact, and generated file input; reject absolute/outside-vault asset sources unless explicitly trusted.

**Scanner validation:** A local ZIP proof exported a public note under a public-only profile and included `Private/secret.txt` in the archive.

**Affected locations:**

- `vulcan-app/src/export.rs:596` (root_control)
- `vulcan-app/src/export.rs:833` (root_control)
- `vulcan-app/src/export/zip.rs:35` (sink)
- `vulcan-app/src/site.rs:4546` (entrypoint)
- `vulcan-app/src/site.rs:4625` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-06`.
- [ ] Add or update a regression test: Export test that denied attachment embeds are omitted or fail.
- [ ] Add or update a regression test: Site build test that absolute/outside asset paths are rejected under default policy.
- [ ] Implement the remediation: Apply the same read filter to every attachment, embed, asset, hover/search artifact, and generated file input; reject absolute/outside-vault asset sources unless explicitly trusted.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-07. Vault-controlled export and site output paths can write or delete outside the vault

- **Finding ID:** `csf_ce327e3aed17aaa14cc89189`
- **Rule:** `path-traversal.export-output`
- **Severity:** `high` (Arbitrary host file write/delete from vault config is a meaningful integrity boundary break.)
- **Confidence:** `high` (Local proofs overwrote an outside-vault export target and deleted a marker inside an absolute site output directory via `--clean`.)
- **Taxonomy:** Arbitrary file write/delete; `CWE-22`

**Summary:** Export profiles and site profiles preserve absolute output paths and join relative paths without rejecting `..`. Export preparation writes to that path, and site builds can remove the configured output directory with `--clean`.

**Root cause:** The invariant should be that vault-controlled profiles cannot select arbitrary host filesystem targets. The implementation trusts profile output paths.

**Reachability:** A malicious vault config can trigger this when a user runs the named export/site profile.

**Data flow:** Vault config output path -> resolver preserves outside path -> write/delete sink.

**Fix direction:** Constrain profile outputs to a vault-owned export/site directory by default; require explicit trusted override for absolute paths; refuse to clean paths outside the resolved project output root.

**Scanner validation:** Local proofs overwrote an outside-vault export target and deleted a marker inside an absolute site output directory via `--clean`.

**Affected locations:**

- `vulcan-app/src/export.rs:2932` (root_control)
- `vulcan-app/src/export.rs:848` (sink)
- `vulcan-app/src/export/zip.rs:19` (sink)
- `vulcan-app/src/site.rs:1207` (root_control)
- `vulcan-app/src/site.rs:364` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-07`.
- [ ] Add or update a regression test: Profile export test rejects absolute and `..` paths.
- [ ] Add or update a regression test: Site clean test refuses an output directory outside the vault.
- [ ] Implement the remediation: Constrain profile outputs to a vault-owned export/site directory by default; require explicit trusted override for absolute paths; refuse to clean paths outside the resolved project output root.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-08. Assistant prompt and skill roots can read host files through config or symlinks

- **Finding ID:** `csf_ea0eec5aea4c765aa26a9aa5`
- **Rule:** `information-disclosure.assistant-roots`
- **Severity:** `high` (The impact is significant because assistant context is explicitly an exfiltration-prone boundary.)
- **Confidence:** `high` (Static trace shows outside reads are loaded into assistant context; no runtime exfil proof was needed because prompt construction is the data sink.)
- **Taxonomy:** Arbitrary file read / secret exfiltration; `CWE-22`, `CWE-200`

**Summary:** Assistant prompt and skill roots are built from vault config and walked with symlink-following filesystem APIs. Files under those roots are read into prompt/skill/agent context, so a malicious vault can include host-readable files in LLM context or trigger recursive symlink traversal.

**Root cause:** The invariant should be that vault assistant context is confined to the vault. The implementation accepts configured roots and symlinks without canonical containment.

**Reachability:** A malicious vault can plant symlinks or path config before a user invokes assistant features.

**Data flow:** Config/symlink root -> recursive file discovery -> `read_to_string` -> assistant context.

**Fix direction:** Canonicalize every assistant root with no-follow traversal, reject roots outside the vault unless trusted, and refuse symlinked files/directories by default.

**Scanner validation:** Static trace shows outside reads are loaded into assistant context; no runtime exfil proof was needed because prompt construction is the data sink.

**Affected locations:**

- `vulcan-core/src/assistant.rs:286` (root_control)
- `vulcan-core/src/assistant.rs:383` (sink)
- `vulcan-core/src/assistant.rs:429` (sink)
- `vulcan-core/src/assistant.rs:770` (root_control)

**Action checklist:**

- [ ] Assign an owner and target release for `H-08`.
- [ ] Add or update a regression test: Assistant context test rejects symlinked prompt file to `/tmp`.
- [ ] Add or update a regression test: Assistant discovery test detects and rejects symlink cycles.
- [ ] Implement the remediation: Canonicalize every assistant root with no-follow traversal, reject roots outside the vault unless trusted, and refuse symlinked files/directories by default.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-09. Vault configuration can exfiltrate env secrets and note text through HTTP backends

- **Finding ID:** `csf_805e852348bf3003f6f1414f`
- **Rule:** `secret-exfiltration.configured-http-backends`
- **Severity:** `high` (This directly crosses the external-network and credential boundary.)
- **Confidence:** `high` (Static trace reaches outbound requests with env-derived bearer keys and note/search payloads. Network validation was not run against external services due restricted network policy.)
- **Taxonomy:** Secret/data exfiltration / SSRF; `CWE-200`, `CWE-918`

**Summary:** Web search and embedding providers load `base_url` and `api_key_env` from vault configuration. Those values are used to send authorization headers, search queries, and embedding note chunks to arbitrary configured endpoints.

**Root cause:** The invariant should be that untrusted vault config cannot choose credential sources or exfiltration endpoints. The implementation treats these fields as trusted provider configuration.

**Reachability:** A malicious vault can set provider URLs before a user runs web search or vector indexing.

**Data flow:** Vault config -> env var lookup -> arbitrary base URL -> HTTP request with query/note text and bearer key.

**Fix direction:** Treat shared vault config as untrusted for provider endpoints and env var names; require local trusted config or an explicit per-vault trust prompt before sending env secrets or note text externally.

**Scanner validation:** Static trace reaches outbound requests with env-derived bearer keys and note/search payloads. Network validation was not run against external services due restricted network policy.

**Affected locations:**

- `vulcan-core/src/web.rs:92` (entrypoint)
- `vulcan-core/src/web.rs:440` (root_control)
- `vulcan-core/src/web.rs:150` (sink)
- `vulcan-core/src/vector.rs:1415` (entrypoint)
- `vulcan-core/src/vector.rs:1521` (root_control)
- `vulcan-embed/src/openai_compat.rs:195` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-09`.
- [ ] Add or update a regression test: Config trust test that shared vault config cannot set `api_key_env` or `base_url` for network providers.
- [ ] Add or update a regression test: Vector/web integration test verifies untrusted config is rejected before outbound request.
- [ ] Implement the remediation: Treat shared vault config as untrusted for provider endpoints and env var names; require local trusted config or an explicit per-vault trust prompt before sending env secrets or note text externally.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-10. Saved reports and automation bypass selected read permission profiles

- **Finding ID:** `csf_0a563999d13601ba480f8adc`
- **Rule:** `authorization-bypass.saved-report`
- **Severity:** `high` (This is a direct authorization bypass for a reusable automation surface.)
- **Confidence:** `high` (Static code trace identifies the caller filter and the unfiltered saved-report sinks; no counter-check is visible on this path.)
- **Taxonomy:** Authorization bypass / information disclosure; `CWE-862`, `CWE-200`

**Summary:** Permission-filter plumbing exists for normal query/search paths, but saved report execution and automation call search/query/Bases evaluators without passing the active read filter. Restricted-profile users can receive or export rows outside their allowed scope.

**Root cause:** The invariant should be that saved reports execute under the caller profile. The implementation bypasses the profile on the saved-report path.

**Reachability:** A restricted user or automation job can run a preexisting saved report that queries private notes.

**Data flow:** Restricted CLI profile -> saved report/automation -> unfiltered search/query/Bases -> output/export.

**Fix direction:** Thread the selected `PermissionFilter` through every saved-report execution mode, including Bases evaluation and exports.

**Scanner validation:** Static code trace identifies the caller filter and the unfiltered saved-report sinks; no counter-check is visible on this path.

**Affected locations:**

- `vulcan-cli/src/lib.rs:630` (root_control)
- `vulcan-cli/src/lib.rs:3205` (entrypoint)
- `vulcan-cli/src/lib.rs:1752` (sink)
- `vulcan-cli/src/lib.rs:2063` (entrypoint)

**Action checklist:**

- [ ] Assign an owner and target release for `H-10`.
- [ ] Add or update a regression test: Saved-report fixture under read-restricted profile returns only allowed notes.
- [ ] Add or update a regression test: Automation export test verifies denied notes are absent.
- [ ] Implement the remediation: Thread the selected `PermissionFilter` through every saved-report execution mode, including Bases evaluation and exports.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-11. Bulk mutation APIs accept `../` and absolute paths before writing

- **Finding ID:** `csf_a63ec0558ffaf5d223e7adf4`
- **Rule:** `path-traversal.bulk-mutation`
- **Severity:** `high` (Arbitrary file write outside the vault is high-impact integrity compromise.)
- **Confidence:** `high` (Static traces from CLI and core agree on source-to-sink; the path helpers do not reject parent traversal on these stdin paths.)
- **Taxonomy:** Path traversal / arbitrary file write; `CWE-22`

**Summary:** CLI `--stdin` path readers and core bulk refactor/suggestion APIs accept caller-supplied note paths, join them with `vault_root`, and later write mutation plans without canonical containment or no-follow checks. `../` and absolute paths can target files outside the vault.

**Root cause:** The invariant should be that bulk note mutation is confined to valid vault notes. The implementation trusts caller strings and cache paths at write time.

**Reachability:** Automation or CLI users can feed path lists to bulk update/refactor commands.

**Data flow:** Stdin/API path string -> `vault_root.join` -> read/modify/write.

**Fix direction:** Normalize every bulk input through `normalize_relative_input_path`, reject absolute and parent components, canonicalize existing files, and reject symlinks before reading/writing.

**Scanner validation:** Static traces from CLI and core agree on source-to-sink; the path helpers do not reject parent traversal on these stdin paths.

**Affected locations:**

- `vulcan-cli/src/lib.rs:1640` (entrypoint)
- `vulcan-core/src/refactor.rs:410` (entrypoint)
- `vulcan-core/src/refactor.rs:573` (sink)
- `vulcan-core/src/suggestions.rs:876` (entrypoint)
- `vulcan-core/src/suggestions.rs:1631` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-11`.
- [ ] Add or update a regression test: CLI test that stdin path `../outside.md` is rejected for query update and refactor rewrite.
- [ ] Add or update a regression test: Core tests for `bulk_set_property_on_paths` and `bulk_replace_on_paths` reject absolute paths and symlinks.
- [ ] Implement the remediation: Normalize every bulk input through `normalize_relative_input_path`, reject absolute and parent components, canonicalize existing files, and reject symlinks before reading/writing.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-12. Vault note and state-file writes follow symlinks outside the vault

- **Finding ID:** `csf_ee34212426aaa69e04285b6c`
- **Rule:** `path-traversal.vault-symlink-write`
- **Severity:** `high` (The impact spans many user-facing mutating commands and breaks the vault containment model.)
- **Confidence:** `high` (Local proofs showed `note append`/`note set` and `config set` modifying symlink targets under `/tmp`; tool lint also modified/chmodded symlink targets.)
- **Taxonomy:** Arbitrary file write via symlink; `CWE-59`, `CWE-22`

**Summary:** Multiple note and `.vulcan` state write paths validate only lexical vault paths or use fixed filenames, then call ordinary `fs::write`/SQLite open operations. A malicious vault can place symlinks at note paths, config files, reports, REPL history, or `.vulcan` itself to redirect writes outside the vault.

**Root cause:** The invariant should be that vault mutation cannot affect files outside the vault. The implementation does not use no-follow opens or canonical containment checks on write targets.

**Reachability:** A user opening or mutating a malicious vault can trigger the symlinked write path.

**Data flow:** Malicious symlink inside vault -> normal write/open path -> outside target modified.

**Fix direction:** Use descriptor-relative, no-follow file operations for vault writes; reject symlinked `.vulcan`; canonicalize parent directories under the vault; add a central secure-write helper.

**Scanner validation:** Local proofs showed `note append`/`note set` and `config set` modifying symlink targets under `/tmp`; tool lint also modified/chmodded symlink targets.

**Affected locations:**

- `vulcan-app/src/notes.rs:254` (sink)
- `vulcan-app/src/notes.rs:390` (sink)
- `vulcan-app/src/config.rs:575` (sink)
- `vulcan-core/src/paths.rs:198` (root_control)
- `vulcan-core/src/paths.rs:215` (sink)
- `vulcan-core/src/saved_queries.rs:173` (sink)

**Action checklist:**

- [x] Add descriptor-relative no-follow read/write/create primitives in `vulcan-core::paths`, with traversal, final-symlink, and intermediate-symlink regression tests.
- [x] Migrate `vulcan-app` note create/set/append/patch and shared/local config mutation writes to the secure path primitives; add hostile note-patch and config-set symlink tests that verify the outside target is unchanged.
- [ ] Assign an owner and target release for `H-12`.
- [ ] Add or update a regression test: Fixture with note symlink to `/tmp` must fail for create/set/append/patch.
- [ ] Add or update a regression test: Fixture with `.vulcan/config.toml` symlink must fail for config set/import.
- [ ] Implement the remediation: Use descriptor-relative, no-follow file operations for vault writes; reject symlinked `.vulcan`; canonicalize parent directories under the vault; add a central secure-write helper.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-13. Templater include resolves absolute and parent paths outside the vault

- **Finding ID:** `csf_93eb359b9af6efd7442344b7`
- **Rule:** `path-traversal.template-include`
- **Severity:** `high` (Arbitrary file read from template execution is high-impact in an agent/automation context.)
- **Confidence:** `high` (A local proof used `<% tp.file.include("/tmp/...md") %>` and preview returned outside-file contents.)
- **Taxonomy:** Arbitrary file read; `CWE-22`, `CWE-200`

**Summary:** The Templater-compatible include API resolves include paths by joining or accepting the supplied path, appending `.md` when needed, and reading it with `fs::read_to_string`. Absolute and parent-traversal paths are not rejected before the read.

**Root cause:** The invariant should be that template includes read only vault template/note files. The resolver permits outside paths and symlinks.

**Reachability:** A malicious vault template can run when a user previews or creates a note from it.

**Data flow:** Template include string -> weak resolver -> filesystem read -> rendered template output.

**Fix direction:** Constrain include resolution to canonical vault paths, reject absolute and parent paths, and apply read permission checks to includes.

**Scanner validation:** A local proof used `<% tp.file.include("/tmp/...md") %>` and preview returned outside-file contents.

**Affected locations:**

- `vulcan-app/src/templates.rs:1159` (sink)
- `vulcan-app/src/templates.rs:1351` (root_control)
- `vulcan-app/src/templates.rs:1365` (root_control)
- `vulcan-app/src/templates.rs:2098` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-13`.
- [ ] Add or update a regression test: Template preview test rejects absolute include path.
- [ ] Add or update a regression test: Template include symlink-to-outside test fails.
- [ ] Implement the remediation: Constrain include resolution to canonical vault paths, reject absolute and parent paths, and apply read permission checks to includes.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### H-14. Shared vault config can trigger command execution or command rewriting without a trust gate

- **Finding ID:** `csf_934b3e19c9eb5ca00795e329`
- **Rule:** `code-execution.untrusted-config`
- **Severity:** `high` (This breaks the untrusted-vault boundary with code execution and command-control effects.)
- **Confidence:** `high` (Static trace shows vault config to process execution; the alias path reaches arbitrary Vulcan command dispatch. The exact extraction command uses `Command::arg`, so shell injection is not required.)
- **Taxonomy:** Code execution / unsafe configuration trust; `CWE-78`, `CWE-15`

**Summary:** Shared `.vulcan/config.toml` is loaded from the vault and can configure attachment extraction commands that run during scan. It can also define aliases that rewrite benign CLI invocations into arbitrary Vulcan subcommands before argument parsing.

**Root cause:** The invariant should be that executable behavior requires local trust, not shared vault content. The implementation honors shared config for command execution and command rewriting.

**Reachability:** A user who scans or runs common commands in an untrusted vault can trigger config-defined behavior.

**Data flow:** Shared vault config -> extraction command or alias rewrite -> command/process or privileged Vulcan subcommand.

**Fix direction:** Move executable settings and aliases to local trusted config, require explicit trust confirmation per vault before honoring shared executable config, and show the expanded command before alias execution in non-interactive contexts.

**Scanner validation:** Static trace shows vault config to process execution; the alias path reaches arbitrary Vulcan command dispatch. The exact extraction command uses `Command::arg`, so shell injection is not required.

**Affected locations:**

- `vulcan-core/src/scan.rs:277` (entrypoint)
- `vulcan-core/src/scan.rs:592` (entrypoint)
- `vulcan-core/src/extraction.rs:105` (sink)
- `vulcan-cli/src/lib.rs:2254` (entrypoint)
- `vulcan-cli/src/lib.rs:2273` (root_control)
- `vulcan-cli/src/lib.rs:2295` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `H-14`.
- [ ] Add or update a regression test: Untrusted vault scan test refuses extraction commands from shared config.
- [ ] Add or update a regression test: CLI test that shared config aliases are ignored unless the vault is trusted.
- [ ] Implement the remediation: Move executable settings and aliases to local trusted config, require explicit trust confirmation per vault before honoring shared executable config, and show the expanded command before alias execution in non-interactive contexts.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

## Medium Severity Findings

### M-01. `web fetch --save` writes to arbitrary filesystem paths

- **Finding ID:** `csf_636ccc9dec08b43e4363da85`
- **Rule:** `path-traversal.web-fetch-save`
- **Severity:** `medium` (The impact is a direct write outside the vault, but currently limited to explicit CLI/automation save usage.)
- **Confidence:** `high` (Static source trace reaches `fs::write` with the unmodified caller path; MCP schema does not expose `save`, so reachability is CLI/automation only.)
- **Taxonomy:** Arbitrary file write; `CWE-22`

**Summary:** The CLI web fetch command checks only network permission before forwarding the caller-provided `--save` path. The app layer creates parent directories and writes fetched bytes to that exact `PathBuf` without vault containment or write-permission checks.

**Root cause:** The invariant should be that a network permission does not imply arbitrary write permission. The implementation treats saved output as outside the permission model.

**Reachability:** A lower-trust automation path that can invoke CLI web fetch can write files even when only network access was intended.

**Data flow:** CLI `--save` path -> app fetch report -> parent creation and write.

**Fix direction:** Require write permission and vault-contained output by default; add an explicit unsafe absolute-output flag if needed.

**Scanner validation:** Static source trace reaches `fs::write` with the unmodified caller path; MCP schema does not expose `save`, so reachability is CLI/automation only.

**Affected locations:**

- `vulcan-cli/src/commands/runtime.rs:398` (entrypoint)
- `vulcan-cli/src/commands/runtime.rs:403` (entrypoint)
- `vulcan-app/src/web.rs:108` (sink)
- `vulcan-app/src/web.rs:118` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-01`.
- [ ] Add or update a regression test: CLI test that `--permissions` denying write blocks `web fetch --save`.
- [ ] Add or update a regression test: CLI test that absolute and `..` save paths are rejected by default.
- [ ] Implement the remediation: Require write permission and vault-contained output by default; add an explicit unsafe absolute-output flag if needed.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-02. MCP HTTP allocates unbounded request bodies before authentication

- **Finding ID:** `csf_34bcb9f75f5626ce96f12eff`
- **Rule:** `denial-of-service.mcp-preauth-body`
- **Severity:** `medium` (Pre-auth DoS on a control-plane interface is security-relevant, though impact is availability rather than data compromise.)
- **Confidence:** `high` (Static trace is sufficient: allocation occurs on attacker-controlled `Content-Length` before authentication.)
- **Taxonomy:** Unauthenticated denial of service; `CWE-400`

**Summary:** The MCP HTTP server reads and allocates the entire `Content-Length` body before authentication and security validation. A large unauthenticated request can force memory allocation even when auth is configured.

**Root cause:** The invariant should be that unauthenticated clients cannot force large memory allocations. The parser allocates before auth and lacks a cap.

**Reachability:** Any client that can connect to the MCP HTTP listener can attempt this.

**Data flow:** TCP request -> large Content-Length -> pre-auth allocation -> auth checks later.

**Fix direction:** Enforce a small maximum body size before allocation, authenticate or reject early where possible, and stream/discard over-limit bodies.

**Scanner validation:** Static trace is sufficient: allocation occurs on attacker-controlled `Content-Length` before authentication.

**Affected locations:**

- `vulcan-cli/src/mcp.rs:504` (entrypoint)
- `vulcan-cli/src/mcp.rs:526` (root_control)
- `vulcan-cli/src/mcp.rs:4042` (root_control)
- `vulcan-cli/src/mcp.rs:4048` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-02`.
- [ ] Add or update a regression test: HTTP parser test with oversized Content-Length returns 413 before allocation.
- [ ] Add or update a regression test: Auth-enabled MCP server test rejects oversized unauthenticated bodies.
- [ ] Implement the remediation: Enforce a small maximum body size before allocation, authenticate or reject early where possible, and stream/discard over-limit bodies.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-03. MCP OAuth redirects accept arbitrary raw redirect URIs

- **Finding ID:** `csf_74787657928f3d70f3154680`
- **Rule:** `oauth.open-redirect-header-injection`
- **Severity:** `medium` (The direct impact is medium because full token compromise has additional flow preconditions, but it weakens an authentication boundary.)
- **Confidence:** `high` (Static trace confirms source to `Location` header; token theft impact still depends on OAuth flow details such as approval token, client secret, and PKCE.)
- **Taxonomy:** Open redirect / response header injection; `CWE-601`, `CWE-113`

**Summary:** The MCP OAuth authorize path accepts any non-empty `redirect_uri` for the local issuer client, percent-decodes query parameters, and writes the resulting value into a raw `Location` header. This enables attacker-controlled redirects and potentially header splitting if control bytes survive decoding.

**Root cause:** The invariant should be that OAuth redirect URIs are registered exact values and header values are CR/LF-safe. The implementation has neither control on this path.

**Reachability:** A browser or local client can request the authorize URL when local OAuth support is enabled.

**Data flow:** Authorize query -> decoded redirect_uri -> Location header.

**Fix direction:** Require pre-registered redirect URIs, reject CR/LF and invalid URI schemes, and encode response headers through a safe HTTP writer.

**Scanner validation:** Static trace confirms source to `Location` header; token theft impact still depends on OAuth flow details such as approval token, client secret, and PKCE.

**Affected locations:**

- `vulcan-cli/src/mcp.rs:3205` (entrypoint)
- `vulcan-cli/src/mcp.rs:3589` (root_control)
- `vulcan-cli/src/mcp.rs:3264` (sink)
- `vulcan-cli/src/mcp.rs:4095` (sink)
- `vulcan-cli/src/mcp.rs:4168` (root_control)

**Action checklist:**

- [ ] Assign an owner and target release for `M-03`.
- [ ] Add or update a regression test: Authorize endpoint test rejects unregistered redirect URI.
- [ ] Add or update a regression test: Header injection test with percent-encoded CR/LF is rejected.
- [ ] Implement the remediation: Require pre-registered redirect URIs, reject CR/LF and invalid URI schemes, and encode response headers through a safe HTTP writer.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-04. Loopback serve API exposes vault data without default auth or browser origin checks

- **Finding ID:** `csf_5d2cd34777696b1533216612`
- **Rule:** `authentication.localhost-serve`
- **Severity:** `medium` (Loopback-only exposure lowers likelihood, but the API crosses a private-vault data boundary.)
- **Confidence:** `medium` (Static review confirms auth is optional; severity is limited by loopback exposure and current single-user CLI context.)
- **Taxonomy:** Missing authentication / local service exposure; `CWE-306`, `CWE-346`

**Summary:** The single-vault HTTP server dispatches search and Dataview endpoints without authentication when no token is configured. Loopback binds are allowed unauthenticated and the handler does not enforce Host or Origin checks, leaving the vault API reachable to local processes and browser-driven requests to localhost.

**Root cause:** The invariant should be that browser-reachable local APIs protect private vault data. The implementation treats loopback as sufficient protection.

**Reachability:** A malicious local process or browser page can attempt localhost requests when the user runs `vulcan serve` without a token.

**Data flow:** Local HTTP request -> optional auth bypass when token absent -> search/Dataview route.

**Fix direction:** Require a token by default for all HTTP API routes, enforce Host/Origin checks, and separate static preview from data APIs.

**Scanner validation:** Static review confirms auth is optional; severity is limited by loopback exposure and current single-user CLI context.

**Affected locations:**

- `vulcan-cli/src/serve.rs:162` (root_control)
- `vulcan-cli/src/serve.rs:178` (sink)
- `vulcan-cli/src/serve.rs:199` (root_control)
- `vulcan-cli/src/serve.rs:362` (evidence)

**Action checklist:**

- [ ] Assign an owner and target release for `M-04`.
- [ ] Add or update a regression test: Serve integration test that data endpoints reject unauthenticated requests even on loopback.
- [ ] Add or update a regression test: Browser-origin test for forbidden Origin/Host.
- [ ] Implement the remediation: Require a token by default for all HTTP API routes, enforce Host/Origin checks, and separate static preview from data APIs.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-05. Filtered vector clustering summarizes denied chunks before filtering

- **Finding ID:** `csf_5d4e8c6231854e7f21d001cb`
- **Rule:** `information-disclosure.vector-filter`
- **Severity:** `medium` (Leakage is narrower than full file read, so severity is medium.)
- **Confidence:** `high` (Static trace confirms denied chunk data can feed returned cluster labels/keywords/snippets when a cluster also contains allowed documents.)
- **Taxonomy:** Authorization bypass / information disclosure; `CWE-862`, `CWE-200`

**Summary:** `cluster_vectors_with_filter` first builds full-cluster summaries over all chunks, including keywords and exemplar snippet/path, then filters only assignments and top documents. Mixed clusters can retain summary fields derived from denied notes.

**Root cause:** The invariant should be that filtered reports derive every returned field only from allowed documents. This implementation filters after summary generation.

**Reachability:** A restricted user invoking vector clustering can receive mixed-cluster metadata.

**Data flow:** All chunks -> cluster summary fields -> post-filtered report retains cluster metadata.

**Fix direction:** Apply permission filters before clustering or recompute every returned summary from the filtered member set only.

**Scanner validation:** Static trace confirms denied chunk data can feed returned cluster labels/keywords/snippets when a cluster also contains allowed documents.

**Affected locations:**

- `vulcan-core/src/vector.rs:1219` (entrypoint)
- `vulcan-core/src/vector.rs:1776` (sink)
- `vulcan-core/src/vector.rs:1800` (sink)
- `vulcan-core/src/vector.rs:1284` (root_control)
- `vulcan-core/src/vector.rs:1301` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-05`.
- [x] Add or update a regression test: Fixture with one allowed and one denied note in same cluster returns no denied snippet/path/term.
- [x] Implement the remediation: Apply permission filters before clustering or recompute every returned summary from the filtered member set only.
- [x] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [x] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [x] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [x] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-06. Dataview inline and expression evaluation use unrestricted note lookup

- **Finding ID:** `csf_00f066a5244552035973c2b5`
- **Rule:** `authorization-bypass.dataview-lookup`
- **Severity:** `medium` (The disclosure is metadata/property-scoped, so severity is medium despite strong evidence.)
- **Confidence:** `high` (Two independent shard reviews found the same path; static trace supports reportability. Runtime proof was not needed for confidence.)
- **Taxonomy:** Authorization bypass / information disclosure; `CWE-862`, `CWE-200`

**Summary:** Filtered property queries and inline Dataview expression evaluation load a full note index and pass it into the expression evaluator. Link-field lookups can resolve restricted notes even when the main query rows are permission-filtered.

**Root cause:** The invariant should be that expression lookups respect the same read filter as the caller. The implementation filters rows but not the side lookup used by expressions.

**Reachability:** A user who can read/evaluate a note containing an inline expression can trigger linked restricted-note lookups.

**Data flow:** Restricted query or inline expression -> full note lookup -> linked-note metadata access -> output.

**Fix direction:** Load a permission-filtered note lookup for expression evaluation, and make link-field access fail closed for denied notes.

**Scanner validation:** Two independent shard reviews found the same path; static trace supports reportability. Runtime proof was not needed for confidence.

**Affected locations:**

- `vulcan-core/src/properties.rs:596` (root_control)
- `vulcan-core/src/properties.rs:605` (sink)
- `vulcan-core/src/properties.rs:650` (entrypoint)
- `vulcan-core/src/properties.rs:669` (root_control)
- `vulcan-core/src/expression/eval.rs:402` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-06`.
- [x] Add or update a regression test: Regression test that inline `[[Private]].secret` under a public-only profile returns denied/empty.
- [x] Add or update a regression test: Query test that expression filters cannot use denied note metadata.
- [x] Implement the remediation: Load a permission-filtered note lookup for expression evaluation, and make link-field access fail closed for denied notes.
- [x] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [x] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [x] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [x] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-07. Bases, Kanban, and note move rewrites follow symlinked vault files

- **Finding ID:** `csf_9d46da9021e23186590a7556`
- **Rule:** `path-traversal.plugin-file-symlink`
- **Severity:** `medium` (This is the same containment class as note writes but with narrower trigger paths.)
- **Confidence:** `medium` (Static source review found no containment or no-follow check on these plugin file sinks. Severity is medium because exploitation generally requires inducing specific plugin operations.)
- **Taxonomy:** Arbitrary file write via symlink; `CWE-59`, `CWE-22`

**Summary:** Plugin-compatible mutation paths for `.base` files, Kanban boards, and note moves read/write paths obtained from the vault or cache using ordinary filesystem APIs. Symlinked board/base/note paths can redirect mutation outside the vault.

**Root cause:** The invariant should be that plugin file mutation remains inside the vault. These code paths treat lexical vault paths as safe filesystem paths.

**Reachability:** A malicious vault can include symlinked plugin files and induce a user to edit a view/board or move a note.

**Data flow:** Symlinked plugin/note path -> read/modify/write sink.

**Fix direction:** Apply the same secure no-follow write helper to Bases, Kanban, and move-rewrite paths; reject symlinked plugin documents during scan or mutation.

**Scanner validation:** Static source review found no containment or no-follow check on these plugin file sinks. Severity is medium because exploitation generally requires inducing specific plugin operations.

**Affected locations:**

- `vulcan-core/src/bases.rs:588` (sink)
- `vulcan-core/src/bases.rs:693` (sink)
- `vulcan-core/src/kanban.rs:323` (sink)
- `vulcan-core/src/kanban.rs:393` (sink)
- `vulcan-core/src/move_rewrite.rs:202` (sink)
- `vulcan-core/src/move_rewrite.rs:214` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-07`.
- [ ] Add or update a regression test: Symlinked `.base` edit test fails without modifying target.
- [ ] Add or update a regression test: Symlinked Kanban board mutation test fails safely.
- [ ] Implement the remediation: Apply the same secure no-follow write helper to Bases, Kanban, and move-rewrite paths; reject symlinked plugin documents during scan or mutation.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-08. Plugin command handlers skip selected permission profiles

- **Finding ID:** `csf_e83391938f0ba0a8f4e3cf78`
- **Rule:** `authorization-bypass.plugin-commands`
- **Severity:** `medium` (This is a real authorization bug, but scope is limited to command paths and plugin objects.)
- **Confidence:** `medium` (Shard reviews traced unguarded handlers to read/write sinks; severity is medium because exploitation requires a restricted CLI/tool caller reaching those command groups.)
- **Taxonomy:** Authorization bypass; `CWE-862`

**Summary:** Several plugin-compatible CLI command handlers call core read/write operations directly without selecting and enforcing the active permission guard. Restricted profiles can read or mutate Kanban boards, Bases views, or periodic notes through these command paths.

**Root cause:** The invariant should be that every CLI command honors `--permissions`. These plugin command handlers omit the shared guard while their sinks read or mutate vault files.

**Reachability:** A lower-trust automation or MCP wrapper exposing these commands can invoke them under a restrictive profile.

**Data flow:** Restricted CLI invocation -> plugin command handler -> unguarded core read/write.

**Fix direction:** Require every command handler to obtain a selected permission guard and check read/write/refactor access for all affected paths before calling core operations.

**Scanner validation:** Shard reviews traced unguarded handlers to read/write sinks; severity is medium because exploitation requires a restricted CLI/tool caller reaching those command groups.

**Affected locations:**

- `vulcan-cli/src/commands/kanban.rs:64` (entrypoint)
- `vulcan-cli/src/commands/kanban.rs:96` (sink)
- `vulcan-cli/src/commands/periodic.rs:112` (entrypoint)
- `vulcan-cli/src/commands/periodic.rs:457` (sink)
- `vulcan-cli/src/commands/bases.rs:384` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-08`.
- [ ] Add or update a regression test: CLI tests that Kanban/Bases/periodic mutating commands fail under read-only profiles.
- [ ] Add or update a regression test: Negative tests for denied board/view paths.
- [ ] Implement the remediation: Require every command handler to obtain a selected permission guard and check read/write/refactor access for all affected paths before calling core operations.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-09. Cache-controlled table names are interpolated into `execute_batch` SQL

- **Finding ID:** `csf_a3e36fe53e5a66a62ea9f434`
- **Rule:** `sql-injection.cache-metadata`
- **Severity:** `medium` (The impact is material but constrained by the rebuildable cache trust boundary.)
- **Confidence:** `medium` (Static trace validates the SQL injection primitive. Impact requires a tampered `.vulcan/cache.db`, which lowers severity because the cache is rebuildable.)
- **Taxonomy:** SQL injection / cache poisoning; `CWE-89`

**Summary:** Cache cleanup and sqlite-vec registry operations read table names from attacker-modifiable cache metadata and interpolate them inside bracket-quoted SQL strings without escaping `]`. A poisoned cache can execute additional SQL when cleanup/drop paths run.

**Root cause:** The invariant should be that persisted cache metadata never controls SQL syntax. The implementation reuses persisted identifiers without revalidation or escaping.

**Reachability:** A malicious vault distribution can include a poisoned `.vulcan/cache.db` or a local attacker can tamper with it before cleanup/vector operations.

**Data flow:** Poisoned cache table_name -> formatted SQL string -> `execute_batch`.

**Fix direction:** Never interpolate persisted identifiers. Revalidate table names against the generated safe-name grammar or store model keys and regenerate table names before SQL; escape identifiers with a dedicated helper.

**Scanner validation:** Static trace validates the SQL injection primitive. Impact requires a tampered `.vulcan/cache.db`, which lowers severity because the cache is rebuildable.

**Affected locations:**

- `vulcan-core/src/cache/schema.rs:771` (entrypoint)
- `vulcan-core/src/cache/schema.rs:779` (sink)
- `vulcan-embed/src/sqlite_vec.rs:87` (entrypoint)
- `vulcan-embed/src/sqlite_vec.rs:109` (sink)
- `vulcan-embed/src/sqlite_vec.rs:497` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-09`.
- [ ] Add or update a regression test: Poisoned cache test with `]` in table name must not execute injected SQL.
- [ ] Add or update a regression test: sqlite-vec registry test rejects invalid stored table names.
- [ ] Implement the remediation: Never interpolate persisted identifiers. Revalidate table names against the generated safe-name grammar or store model keys and regenerate table names before SQL; escape identifiers with a dedicated helper.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-10. Git commands can access paths outside a vault nested in a larger worktree

- **Finding ID:** `csf_a4aa28feca508d872a208277`
- **Rule:** `path-traversal.git-pathspec`
- **Severity:** `medium` (The issue is medium because it requires git permission and a nested-worktree layout.)
- **Confidence:** `medium` (Static trace confirms the pathspec gap; command injection is not present because argv and `--` are used.)
- **Taxonomy:** Path traversal / unintended repository access; `CWE-22`

**Summary:** Git helper functions normalize paths by replacing separators and stripping `./`, but they do not reject absolute or parent paths before passing them to `git -C vault -- <path>` or joining them for auto-commit checks. If the vault is a subdirectory of a larger Git worktree, `../sibling` pathspecs can expose or stage files outside the vault.

**Root cause:** The invariant should be that vault git operations are confined to the vault root. Git pathspecs are repository-relative concepts and can escape a nested vault without explicit containment checks.

**Reachability:** A caller with git command access in a nested vault can request sibling paths.

**Data flow:** Caller path with `..` -> weak normalization -> git pathspec or joined path -> outside-vault repository file.

**Fix direction:** Reject absolute and parent components before git pathspec use, and resolve paths relative to the vault root with canonical containment before staging/committing.

**Scanner validation:** Static trace confirms the pathspec gap; command injection is not present because argv and `--` are used.

**Affected locations:**

- `vulcan-core/src/git.rs:207` (entrypoint)
- `vulcan-core/src/git.rs:341` (entrypoint)
- `vulcan-core/src/git.rs:556` (root_control)
- `vulcan-core/src/git.rs:350` (sink)
- `vulcan-core/src/git.rs:519` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-10`.
- [ ] Add or update a regression test: Git command test in nested worktree rejects `../sibling.md`.
- [ ] Add or update a regression test: Auto-commit test ignores outside-vault status paths.
- [ ] Implement the remediation: Reject absolute and parent components before git pathspec use, and resolve paths relative to the vault root with canonical containment before staging/committing.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-11. Static search export emits all indexed content without a permission filter

- **Finding ID:** `csf_8abea41c0cd7fdcb5ea0400a`
- **Rule:** `information-disclosure.static-search-export`
- **Severity:** `medium` (Publication leaks are real, but impact depends on integration path.)
- **Confidence:** `high` (Static trace validates the leak; severity is medium because reachability depends on the caller wiring this export into site/public output.)
- **Taxonomy:** Information disclosure; `CWE-200`, `CWE-862`

**Summary:** The static search index export has only `paths` input, loads every indexed entry from `search_chunk_content`, and returns note content, titles, and aliases without any permission-filter clause. If used for a public site or restricted export, it can expose private notes.

**Root cause:** The invariant should be that derived publication indexes contain only authorized public data. The export has no permission parameter.

**Reachability:** A site/export workflow using this helper can publish the full search index.

**Data flow:** Search cache -> unfiltered static index export -> public/restricted artifact.

**Fix direction:** Add a `PermissionFilter`/publication filter parameter to static search export and require site/export callers to pass it.

**Scanner validation:** Static trace validates the leak; severity is medium because reachability depends on the caller wiring this export into site/public output.

**Affected locations:**

- `vulcan-core/src/search.rs:398` (entrypoint)
- `vulcan-core/src/search.rs:403` (root_control)
- `vulcan-core/src/search.rs:429` (sink)
- `vulcan-core/src/search.rs:454` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-11`.
- [x] Add or update a regression test: Static search export fixture with private note under public filter omits private content.
- [x] Implement the remediation: Add a `PermissionFilter`/publication filter parameter to static search export and require site/export callers to pass it.
- [x] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [x] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [x] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [x] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-12. MCP task and suggestion read tools bypass read filters

- **Finding ID:** `csf_9597c3a37b144e7e4b886747`
- **Rule:** `authorization-bypass.mcp-read-tools`
- **Severity:** `medium` (The data is metadata/task-scoped, so severity is medium.)
- **Confidence:** `medium` (Static trace compares these paths to search/query handlers that do pass `self.guard.read_filter()`, making the bypass concrete.)
- **Taxonomy:** Authorization bypass / information disclosure; `CWE-862`, `CWE-200`

**Summary:** MCP exposes `suggest_links`, `task_list`, and `task_query` as read-visible tools, but their handlers call report generation without passing the active guard read filter. Restricted MCP clients can receive task/link metadata from denied notes.

**Root cause:** The invariant should be that MCP read-visible tools still respect path-scoped read filters. The handlers skip the filter on these tools.

**Reachability:** A restricted MCP client can call these tools when the catalog exposes them.

**Data flow:** Restricted MCP client -> read-visible tool -> unfiltered task/suggestion report.

**Fix direction:** Thread `self.guard.read_filter()` into every MCP read tool implementation and add catalog tests that every read-visible tool has a filtered sink.

**Scanner validation:** Static trace compares these paths to search/query handlers that do pass `self.guard.read_filter()`, making the bypass concrete.

**Affected locations:**

- `vulcan-cli/src/mcp.rs:2063` (entrypoint)
- `vulcan-cli/src/mcp.rs:2073` (entrypoint)
- `vulcan-cli/src/mcp.rs:2094` (entrypoint)
- `vulcan-cli/src/mcp/catalog.rs:304` (root_control)
- `vulcan-cli/src/mcp/catalog.rs:690` (root_control)

**Action checklist:**

- [ ] Assign an owner and target release for `M-12`.
- [ ] Add or update a regression test: MCP fixture where `task_list` under restricted profile excludes denied note tasks.
- [ ] Add or update a regression test: Static/unit test for catalog-to-handler filter coverage.
- [ ] Implement the remediation: Thread `self.guard.read_filter()` into every MCP read tool implementation and add catalog tests that every read-visible tool has a filtered sink.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-13. Site preview server serves symlinked files outside the output directory

- **Finding ID:** `csf_c278c129bf49d0468dca035a`
- **Rule:** `path-traversal.site-server-symlink`
- **Severity:** `medium` (The impact is file read but with local preview constraints.)
- **Confidence:** `medium` (Static trace validates the symlink read. Severity is medium because the server is loopback-preview oriented and requires a symlink in output.)
- **Taxonomy:** Path traversal / arbitrary file read; `CWE-22`, `CWE-59`

**Summary:** The site preview server rejects lexical `..` URL traversal, but it checks `is_file()` and then reads the candidate path without canonicalizing it back under the output root. A symlink inside the output directory can serve host files to preview clients.

**Root cause:** The invariant should be that served files are contained in the generated output directory. Lexical URL checks do not stop symlink escape.

**Reachability:** A local preview client can request the symlink path after a malicious build/output setup.

**Data flow:** HTTP path -> output_dir join -> symlink-following file check/read -> response.

**Fix direction:** Use `symlink_metadata`, reject symlinks, canonicalize the final path, and verify it remains under the output root before reading.

**Scanner validation:** Static trace validates the symlink read. Severity is medium because the server is loopback-preview oriented and requires a symlink in output.

**Affected locations:**

- `vulcan-cli/src/site_server.rs:534` (root_control)
- `vulcan-cli/src/site_server.rs:542` (root_control)
- `vulcan-cli/src/site_server.rs:555` (root_control)
- `vulcan-cli/src/site_server.rs:390` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-13`.
- [ ] Add or update a regression test: Preview server test rejects a symlink inside output pointing to `/tmp/secret`.
- [ ] Implement the remediation: Use `symlink_metadata`, reject symlinks, canonicalize the final path, and verify it remains under the output root before reading.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-14. Frontmatter redaction can miss valid YAML keys during publication transforms

- **Finding ID:** `csf_097899e2675477833ec5e9ac`
- **Rule:** `information-disclosure.frontmatter-redaction`
- **Severity:** `medium` (The impact is publication leakage of fields the operator explicitly asked to remove.)
- **Confidence:** `high` (Static trace is deterministic and sufficient; exploitability depends on users relying on frontmatter redaction for public output.)
- **Taxonomy:** Information disclosure; `CWE-200`, `CWE-116`

**Summary:** The content transform parses YAML keys to decide what to exclude, then removes lines only when the raw text starts with `key:`. Valid YAML forms such as quoted keys can be selected by parsed key name but remain in the emitted content when the textual matcher fails.

**Root cause:** The invariant should be that requested redaction removes all syntactically valid instances of the selected YAML key. The implementation mixes parsed key selection with brittle textual deletion.

**Reachability:** A note author can format sensitive keys as quoted YAML and bypass publication redaction.

**Data flow:** Valid YAML quoted key -> parsed as redaction target -> textual matcher misses -> published content retains key.

**Fix direction:** Perform redaction on the parsed YAML document or preserve source spans from a YAML parser; fail closed if a selected key cannot be removed from source text.

**Scanner validation:** Static trace is deterministic and sufficient; exploitability depends on users relying on frontmatter redaction for public output.

**Affected locations:**

- `vulcan-core/src/content_transforms.rs:139` (root_control)
- `vulcan-core/src/content_transforms.rs:157` (root_control)
- `vulcan-core/src/content_transforms.rs:465` (sink)
- `vulcan-core/src/content_transforms.rs:501` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-14`.
- [ ] Add or update a regression test: Transform test where quoted sensitive key is removed.
- [ ] Add or update a regression test: Transform test fails closed when source span cannot be matched.
- [ ] Implement the remediation: Perform redaction on the parsed YAML document or preserve source spans from a YAML parser; fail closed if a selected key cannot be removed from source text.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-15. Attachment extraction buffers unlimited command output before applying caps

- **Finding ID:** `csf_4dfddeb5761dfe90ae0114d0`
- **Rule:** `denial-of-service.extraction-output`
- **Severity:** `medium` (The issue is availability-focused but realistic on an indexing workflow.)
- **Confidence:** `high` (Static trace validates the allocation order; no runtime PoC was run because it would intentionally stress memory.)
- **Taxonomy:** Resource exhaustion; `CWE-400`

**Summary:** Attachment extraction uses `Command::output()` to capture stdout/stderr fully in memory, then applies configured max-output truncation afterward. A configured extractor or malicious attachment can produce unbounded output and exhaust memory before the cap runs.

**Root cause:** The invariant should be that extraction output limits bound memory use. The implementation enforces the limit after unbounded buffering.

**Reachability:** A malicious vault config or attachment can trigger extraction during scan.

**Data flow:** Extractor process -> unbounded stdout capture -> later truncation.

**Fix direction:** Stream child stdout with a hard byte limit and kill the child when the limit or timeout is exceeded; cap stderr too.

**Scanner validation:** Static trace validates the allocation order; no runtime PoC was run because it would intentionally stress memory.

**Affected locations:**

- `vulcan-core/src/extraction.rs:90` (root_control)
- `vulcan-core/src/extraction.rs:105` (entrypoint)
- `vulcan-core/src/extraction.rs:116` (sink)
- `vulcan-core/src/extraction.rs:128` (sink)
- `vulcan-core/src/extraction.rs:141` (root_control)

**Action checklist:**

- [ ] Assign an owner and target release for `M-15`.
- [ ] Add or update a regression test: Extractor test with output over limit terminates without allocating the whole output.
- [ ] Add or update a regression test: Timeout/kill test for never-ending extractor output.
- [ ] Implement the remediation: Stream child stdout with a hard byte limit and kill the child when the limit or timeout is exceeded; cap stderr too.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

### M-16. Parser, query, and expression engines contain unbounded recursion and allocation paths

- **Finding ID:** `csf_46803ce46f946c0f8f67cffb`
- **Rule:** `denial-of-service.parser-expression`
- **Severity:** `medium` (Availability impact is meaningful but not a direct confidentiality/integrity compromise, so severity is medium.)
- **Confidence:** `medium` (Static traces from multiple shards identify deterministic superlinear or unbounded allocation paths. Runtime stress tests were not run to avoid intentionally exhausting resources.)
- **Taxonomy:** Resource exhaustion; `CWE-400`

**Summary:** Multiple untrusted-content parsers and evaluators have superlinear scans, recursion without depth limits, or unbounded string allocation. Examples include DQL/expression tokenizer wikilink lookahead, Dataview list-fragment reparsing, block-ref reverse scans, string repeat/pad/toFixed allocations, and Tasks recurrence/query expansion.

**Root cause:** The invariant should be that hostile vault content and query text are bounded by depth, size, and output budgets. Several engines lack those budgets.

**Reachability:** A malicious vault or lower-trust API caller can submit crafted content to indexing/query surfaces.

**Data flow:** Hostile note/query/task content -> parser/evaluator loops or allocations -> process resource exhaustion.

**Fix direction:** Add shared input-size, recursion-depth, node-count, output-size, and timeout budgets to parser/evaluator entrypoints; replace repeated scans with linear algorithms.

**Scanner validation:** Static traces from multiple shards identify deterministic superlinear or unbounded allocation paths. Runtime stress tests were not run to avoid intentionally exhausting resources.

**Affected locations:**

- `vulcan-core/src/dql/token.rs:98` (root_control)
- `vulcan-core/src/expression/token.rs:94` (root_control)
- `vulcan-core/src/parser/dataview.rs:136` (root_control)
- `vulcan-core/src/parser/block_ref.rs:7` (root_control)
- `vulcan-core/src/expression/eval.rs:859` (sink)
- `vulcan-core/src/tasks/recurrence.rs:170` (sink)

**Action checklist:**

- [ ] Assign an owner and target release for `M-16`.
- [ ] Add or update a regression test: Fuzz/regression tests for unterminated `[[[[` tokenization.
- [ ] Add or update a regression test: Regression test for Dataview fragment recursion and capped string repeat/toFixed/pad lengths.
- [ ] Implement the remediation: Add shared input-size, recursion-depth, node-count, output-size, and timeout budgets to parser/evaluator entrypoints; replace repeated scans with linear algorithms.
- [ ] Audit adjacent command handlers, MCP tools, app workflows, and tests for the same boundary issue.
- [ ] Run the narrowest relevant test target, then `cargo test --workspace` before closing.
- [ ] Re-run the original reproduction or security scan and attach the verification evidence to the tracking issue.

**Preventive controls:**

- [ ] Add regression tests that run the affected command under a restrictive permission profile or hostile-vault fixture.
- [ ] Centralize containment, permission, and sandbox checks in reusable helpers instead of relying on call-site convention.

## Closure Criteria

A finding should only be marked complete when the code fix, regression tests, and verification evidence are all present. For path, permission, sandbox, and publication fixes, include at least one hostile fixture that would have failed before the change.

Final closure requires a fresh deep scan with no remaining high findings, dependency/advisory scanners run with current tooling, and a documented decision for any accepted residual medium-risk items.
