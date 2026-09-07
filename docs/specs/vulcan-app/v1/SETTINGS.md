# Vulcan App settings v1

Status: normative Phase 19 implementation target; runtime implementation is pending.
Revision: 2026-09-07 pre-release additive settings review. Existing manifests and identity
fixtures remain valid without changes. Older closed validators reject the new declaration
and capabilities; hosts MUST negotiate the `app.settings.v1` feature before use. This does
not claim deployed compatibility or enable a method before its protocol fixtures pass.

## Declaration and ownership

An optional manifest `settings` object declares an independently versioned JSON Schema
payload and an exhaustive map of top-level setting keys to permitted storage scopes:

```json
{
  "settings": {
    "version": 1,
    "schema": "schemas/settings.json",
    "scopes": {
      "output-folder": "shared",
      "page-size": "shared-and-local",
      "theme": "local"
    }
  }
}
```

`shared` means vault-global for this AppId, across every instance on every synchronized
device. `local` means this app and vault on the executing device. `shared-and-local`
permits a shared value and a device override. Settings are not global across unrelated
apps or vaults. Existing `configuration` remains instance-specific; it neither replaces
nor overlays this settings namespace. Apps may explicitly use both for different purposes.

The schema MUST be an inventoried `application/schema+json` payload. Its root is a closed
object with explicit `properties`; property names exactly match `scopes`, use manifest
local-ID syntax, and number at most 256. Unknown keys, unsupported validation keywords,
remote references, invalid defaults, or unresolved packaged references are diagnostics
that block acceptance, not ignored constraints. Reuse the platform's bounded JSON Schema
profile; do not fetch schemas. Values follow App API v1's JSON and exact-number rules.
Nested objects and arrays are permitted but inherit their top-level key's scope.
Bound each layer and resolved object to 256 KiB of UTF-8 JSON and nesting depth 32;
validate these limits before materializing or rendering values.

Use ordinary JSON Schema `title`, `description`, `enum`, bounds, `required`, and `default`
for validation and host presentation. A default exists only when explicitly declared on
the top-level property's schema and must validate as that complete property's value.
The host does not recursively invent defaults. Presentation text is untrusted plain text,
never HTML, terminal escape sequences, executable widgets, or authority. An omitted settings
declaration exposes no keys and permits no persisted values.

## Canonical and device-local storage

Shared values live at `.vulcan/app-settings/<app-id>.json` in the canonical vault, outside
the ignored `.vulcan/apps/` private-store tree. The envelope is
`{"app_id":"org.example.reader","version":1,"values":{}}` with only those fields.
These files are human-editable, survive cache rebuild and uninstall, participate in
Git/history and file-tree synchronization, and MUST be exempted from broad `.vulcan/`
ignore rules. They are control files, not Markdown notes or default publication content.
Saving writes the file; Git commits remain opt-in and honor `--no-commit`.

Local envelopes have the same shape, but live in protected host-owned configuration
storage outside the synchronized vault, keyed by stable local vault identity and AppId.
They are durable configuration, not cache. Direct CLI and the daemon use the same
allocation for the same vault/device; moving a registered vault preserves that identity.
Unregistered direct operation must use the same durable identity lookup without requiring
a daemon. Local settings never enter Git, publication, sync, or shared backup/export by
default. Explicit local export is a separate administrative choice. App code sees logical
identity and revisions, never host paths.

In a remote session, `local` means the executing daemon device, not the browser, terminal
client, or user's other computers. Reports and the TUI identify that device before a
write. Device-local settings are not user-private preferences on a multi-user daemon;
host configuration authorization still applies. Browser-local UI state belongs in a
separate client-state facility, not an implicit third settings layer.

Neither layer may contain secret values or grant/trust/activation state. Credentials
remain in Phase 17 secret bindings; settings may name an independently authorized binding
but never carry a secret handle or grant. A changed endpoint or path cannot expand an
app's authority. Synchronized configuration is untrusted input and cannot authorize
network access, host execution, installation, or migrations.

## Resolution, validation, and lifecycle

For each key, precedence is explicit schema default, then permitted shared value, then
permitted local value. Each upper layer replaces the whole top-level value: objects are
not deep-merged and arrays are not concatenated. `unset` removes only the selected layer
and reveals the next value; it is not deletion of a default. Null, where the platform
schema profile permits it, is a value, never an unset sentinel.

Each stored layer is a partial object: validate present keys, types, and scopes without
requiring omitted keys. Validate the fully resolved object, including required fields and
cross-field constraints, before invocation and before saving. Shared writes additionally
validate the portable default/shared projection for required keys that allow shared
storage; local-only requirements are checked on each device. One device's override cannot
make an invalid shared value acceptable. A device cannot validate other devices' unseen
overrides: each receiving device revalidates and reports incompatibility before use.

Resolution reports effective values and per-key provenance (`default`, `shared`, `local`,
or `absent`), explicit layer values, schema identity/version, executing-device identity,
and opaque revisions for both layers, including absent-file revisions. Invalid external
edits, merge markers, wrong versions, and invalid local overrides block dependent app
invocation with actionable diagnostics; never silently ignore, rewrite, or fall back to
stale cached settings. Authorized host tools can inspect errors and plan repairs without
running the app. Running invocations use one validated immutable settings snapshot; new
invocations revalidate changed revisions. Settings alone never trigger app code execution.

All instances in a vault share one settings schema identity/version for an AppId.
Activation of another package with a different settings schema digest, scope map, or
version requires a reviewed coordinated update or is blocked; no last-loaded-schema wins.
Updates preview changes to defaults, scopes, and all affected local instances, validate
explicit replacement values, and never copy local-only values into shared files. Schema
changes have no inferred executable migration. An old device receiving a newer version
blocks dependent invocation until compatible code/configuration is explicitly selected.
Uninstall preserves both layers. Explicit administrative deletion is separate from
per-key unset and must identify affected instances and targets.

## API and authorization

App API `settings.describe`, `settings.get`, `settings.plan`, and `settings.apply` are
available through the same typed contracts in QuickJS, WASM host calls, and browser bridge.
The host's administrative API selects a vault and AppId; an app call derives them from its
authenticated instance and cannot select another app. `app.instance.get` must not expose
these settings as a metadata shortcut. Generated HTTP/OpenAPI and SDK contracts follow
the existing App API registry rules, not an independent settings implementation.

`app.settings.read` and `app.settings.write` requests require both `keys` (exact setting
IDs or `*`) and `targets` (`shared`, `local`) selectors. Empty arrays match nothing.
Keys and targets intersect with current caller, installation, instance, entrypoint, and
host configuration permissions; a settings-write capability alone is not administrative
configuration authority. Shared reads/writes additionally require applicable canonical
control-file read/write authority. Ordinary store grants do not authorize settings.

`describe` returns authorized schema properties, scopes, and presentation metadata.
`get` selects `shared`, `local`, or `effective` and optional keys. Effective reads require
read authority for every permitted layer of a requested key, even when a layer is absent;
otherwise deny that key without leaking value, presence, revision, or provenance. Full
schema descriptions and validation errors must not disclose denied keys or constraints.
Host administrative inspection is available for disabled instances; app invocations
retain normal enablement checks.

`plan` accepts one explicit target, a nonempty bounded list of `set`/`unset` operations
on distinct top-level keys, and expected schema/shared/local revisions. `set` carries a
typed JSON value. The service stages the complete result, checks permissions and all
validation dependencies, and returns the old/new target and effective values, affected
instances, warnings, and a plan ID. If cross-field validation needs denied keys, reject
the plan with `permission_denied` rather than returning hidden values in errors.
Read authority over all permitted layers of changed keys, write authority for the selected
target, and read authority over validation dependencies are required. Plans cannot mix
targets or apps.

`apply` accepts the plan ID, accepted revisions, and an idempotency key. It rechecks
authorization, schema, both layer revisions, and instance dependencies before one atomic
file replacement. Changes return `stale_state`; repeated identical apply is idempotent.
Serialize cooperating writers and retain repair evidence for interrupted operations;
external editors remain outside host isolation. No partial key saves. Dry-run creates
no configuration file or persistent plan. Errors use the common App API vocabulary.

## CLI and TUI

Planned commands, all supporting stable JSON reports and direct operation:

```text
vulcan apps settings describe <app-id>
vulcan apps settings show <app-id> [--target shared|local|effective]
vulcan apps settings get <app-id> <key> [--target shared|local|effective]
vulcan apps settings set <app-id> <key> --value-json <json> --target shared|local [--dry-run]
vulcan apps settings unset <app-id> <key> --target shared|local [--dry-run]
vulcan apps settings edit <app-id> --target shared|local [--dry-run]
```

Vault selection uses Vulcan's normal selection rules. Reads default to effective; writes
require a target and never guess. `set` also supports `--value-file <path|->` mutually
exclusive with `--value-json` for typed input without shell quoting. Noninteractive
commands require no prompts. `edit` is an explicitly interactive TUI, like existing
`config edit`; without a TTY it returns `invalid_request` with the headless set/unset
alternative, and `--output json` emits a structured diagnostic without launching a TUI.

The host renders searchable fields with labels/help, type, required status, bounds,
effective value, provenance, override state, selected target, and executing device.
Booleans use toggles, enums selectors, scalar values validated inputs, and structured
values a bounded JSON editor. Unsupported form shapes always have a validated JSON
fallback or an explicit unsupported diagnostic; no fields disappear silently.
Reset means unset in the selected layer, and previews the revealed inherited value.
Target changes are explicit and cannot transfer unsaved values automatically. Save uses
one shared plan/apply batch, shows target and effective diffs, and requires explicit save;
cancel writes nothing. Stale edits retain the draft for reload/review, never overwrite.
Dry-run previews without applying. The TUI requires no app execution or daemon.

## Delivery and acceptance

This revision defines contracts and manifest shape tests, not production settings APIs.
Implement core schema/resolution semantics and app-service persistence with Phase 19.6;
expose CLI alongside instance management and App API with the host bridge. The TUI can
follow without delaying API/headless availability. Before enabling methods, add versioned
request/success/error fixtures and generated-contract conformance as required by SPEC.md.

Runtime acceptance must cover defaults and missing required values; all three scopes;
unknown keys and unsupported schemas; whole-object/array replacement; unset inheritance;
layer and cross-field validation; shared values across two instances/devices and isolation
across apps/vaults; synchronized control files and excluded local files; rebuild/uninstall
preservation; direct/daemon identity parity; remote-device labeling; permission-filtered
schema/value/provenance/errors; revocation, stale plans, idempotency, interrupted atomic
saves, external edits, incompatible schema updates, and local-to-shared scope changes.
CLI tests cover typed file/stdin input, JSON reports, required targets, and dry-run.
TUI tests cover type widgets, JSON fallback, target display, reset, cancel, reviewed batch
save, validation errors, stale draft preservation, and no-TTY behavior.

Skill-impact review: existing `configuration-and-permissions` and `plugin-authoring`
bundled skills describe shipped host configuration and lifecycle plugins. They remain
unchanged for this specification-only revision; add available commands, target/device
semantics, and safe mutation examples to configuration guidance when the feature ships.
No new bundled skill or harness-template change is needed for this design revision.
