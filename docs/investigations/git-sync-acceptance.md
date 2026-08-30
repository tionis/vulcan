# Installed-Git synchronization acceptance

## Scope and decision

This matrix records the completion boundary for Vulcan's first active device/file-tree backend.
The supported engine is the installed Git CLI; direct one-shot operation, daemon supervision, and
the companion protocol all call the same finite application transaction. The matrix does not
promote the optional future gitoxide engine, certify a particular Forgejo deployment, or treat the
passive/process/SilverBullet alternatives in Phase 12.9-12.10 as part of the Git backend.

The local Linux verification command for the current implementation is:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The repository CI runs the full workspace suite and lint gate on Linux, macOS, and Windows. Actual
Forgejo and Android/Termux shared-storage certification remain explicit environment gates below;
typed target-profile tests are not represented as proof that those external environments ran.

## Evidence matrix

| Property | Evidence | Status |
|---|---|---|
| Typed engine and backend contracts | `vulcan-sync::GitEngine`, `SyncBackend`, contract JSON tests, and the public versioned engine conformance harness | complete |
| Ordinary-Git interoperability | conformance fixture creates the seed/bare remote with ordinary Git and verifies engine-produced objects plus live/custom refs through ordinary Git | complete for the CLI engine |
| Capture before remote contact/apply | offline, incompatible-tree, staged-state, cancellation, and bootstrap race tests retain the local candidate before remote/application work | complete |
| Normal-index isolation | alternate capture, structured merge, supplied patch, semantic plumbing, application, and conformance tests compare the untouched normal index | complete |
| Lease-only publication | local ref CAS, remote live/custom-ref push/delete, rejected-push retry, epoch rollover/expiry, resolution, and semantic proposal tests reject stale expected objects | complete |
| Multi-writer behavior | two-worktree merge, rejected winner retry, reordered structured additions, conflict-identity role reversal, offline epoch reconnect, and conflict materialization tests | complete |
| Conflict preservation | immutable record/artifact/ref tests cover text, structured JSON, binary/structural review, deterministic copies, explicit resolution, and original-object retention | complete |
| Whole-tree validation | path, parser/schema, Markdown links, ambiguity, deletion ceiling, exact tree, target platform, and worktree drift tests gate publication/application | complete |
| Recovery state | every recoverable journal phase, apply markers, advisory lock contention/stale files, daemon restart/requeue, watcher overflow, pause/resume, guarded Git-operation markers, missing sync-ref objects, detached Git loss, stale/moved refs, cache refresh/rebuild, and idempotent resolution/apply/reject/retention tests | complete for implemented boundaries |
| Direct/daemon equivalence | daemon jobs call `vulcan-app`'s direct finite transaction; CLI and supervisor tests cover registration revalidation, progress persistence, cancellation, aggregate jobs, and daemon absence | complete |
| Companion security and protocol | transport-neutral service tests, loopback HTTP/CORS/origin/version/bearer/idempotency tests, authenticated WebSocket tests, and the Obsidian mock-daemon suite | complete |
| CLI and automation surfaces | CLI smoke tests and the composite command schema cover clone/recovery, every sync command, JSON reports, completion/describe projections, read-only MCP sync tools, and permission profiles | complete |
| Semantic history | deterministic top-level/file/change/hunk/all grouping, provider-backed whole-file plans, exact intermediate/final trees, stale apply, rejection, and proposal-ref leases | complete for the bounded initial contract |
| Agent escalation | fake-provider tests cover exact inputs, focused/broad permission-filtered context, tools, cancellation, malformed output, stale inputs, proposal isolation, redacted audit, explicit rejection/approval, and local opt-in auto-accept | complete |
| Agent guidance | managed `sync-workflow`, `git-workflow`, `configuration-and-permissions`, and `diagnostics-and-repair` payload/install/discovery/refresh/collision tests | complete |

## Residual gates, not installed-Git implementation gaps

- **Forgejo custom refs:** run and record every item in
  `docs/investigations/forgejo-custom-refs.md` against the deployed server. Until then, version 1
  intentionally publishes the hidden branch namespace and keeps custom refs local/test-only.
- **Windows certification:** the checked-in CI matrix runs the suite on `windows-latest`; a release
  should retain the successful run URL/artifact for the exact candidate commit. Platform-policy
  unit tests remain host-independent but are not a substitute for that run.
- **Android/Termux certification:** run clone, status, doctor, push/pull, conflict review, detached
  recovery, path portability, symlink/link-file, and uninstall-loss procedures on a real supported
  Android/Termux/shared-storage combination. The current Linux integration fixture proves the
  detached one-shot workflow semantics, not Android filesystem or lifecycle behavior.
- **gitoxide:** promotion remains post-MVP and requires an exact reviewed `gix` version, explicit
  HTTPS/authentication and repository-feature capabilities, device-local selection/preflight, and
  the same conformance suite on every intended platform. The CLI engine remains the only advertised
  implementation.
- **Additional backends:** passive filesystem observation, `obsidian-headless`, Seafile process
  supervision, SilverBullet full-Space sync, and storage virtualization are separate backend or
  architecture decisions. Their unchecked roadmap items do not weaken the installed-Git backend's
  safety claim.

## Release rule

Do not describe a residual gate as passed from simulated profile tests or design documentation.
Record concrete external evidence for the exact release candidate. A failed or unavailable gate
keeps the conservative fallback: installed Git, hidden live branch, materialized `Path` worktree,
and direct one-shot Termux operation.
