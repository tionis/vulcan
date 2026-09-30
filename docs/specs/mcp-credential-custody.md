# Named MCP Credential Custody

Named foreground and resident hosts use `vulcan-secrets::SecretStore`. This implements
Roadmap 10.10's opaque credential custody, not the native-keychain or device-signing work
in 12.16. Ordinary stdio and advanced direct HTTP flags retain their existing behavior.

## Identity and ownership

The immutable remote instance ULID deterministically selects a lowercase `mcp-<instance>`
namespace. Issuer and signing credentials use `<namespace>.issuer` and `<namespace>.signing`;
each confidential OAuth client uses `<namespace>.client.<blake3(exact client ID)>`. All references
select the closed `file_v1` provider. This mapping is versioned by the provider discriminator and
must not change automatically when other providers become available. Init/show JSON reports only
these references. Copying vault configuration neither installs credentials nor activates a remote.

The protected store is under the device's Vulcan state directory, outside vaults, TOML, Git, and
`cache.db`. Its files are owner-only, immutable through ordinary create, and contain bounded
opaque values. This is exportable, unattended file custody, not encryption, a native keychain,
or hardware protection. Provider errors fail closed without fallback or permission repair.
Mutation locks explicitly release at the operation boundary; cloned or fork-inherited descriptors
cannot retain a completed operation's lock. A competing live mutation still reports `Locked`.

An immutable, owner-only fingerprint receipt for each loaded issuer/signing reference lives in
the separate device-state `mcp-credential-bindings` directory. Once bound, a missing or different
provider value stops startup before either credential can be generated. Preserve these receipts
with credential backups; deleting them is not a supported rotation or recovery procedure.
An existing unbound value (including an interrupted first creation or an upgrade from the earlier
custody format) is adopted without changing its bytes. Receipts cannot detect historical loss
before their first creation, or deliberate deletion of both custody and binding state by the
device owner; they are repair evidence, not protection against filesystem administrators.

OAuth client registry version 2 stores client metadata, a bound namespace, and optional secret
references, never inline client secrets. Public clients declaring `none` have no secret reference.
Confidential clients declare `client_secret_basic` or `client_secret_post` and require the exact
instance/client-derived reference. Unknown providers, namespace/reference substitution, missing
credentials, malformed metadata, and unsupported versions fail closed. Registry publication remains
atomic and cross-process locked; every lookup reloads durable state. An older/direct version-1
reader refuses version 2 rather than silently ignoring credentials.

## Explicit legacy migration

Stop the instance, then run:

```sh
vulcan mcp remote migrate-credentials <name> --dry-run
vulcan mcp remote migrate-credentials <name>
```

Preview reads source metadata and provider inspection state only: no source secret bytes,
provider creation, registry locks, or publication. Client counts remain unknown during preview.
Apply acquires the same per-instance runtime lock as startup and management and rechecks the
remote definition. It reads only the fixed protected legacy issuer files and client registry;
there is no arbitrary import path or environment-variable ingestion.

Issuer values preserve the legacy loader's UTF-8/trim semantics; confidential client values
preserve their exact bytes and client identities. Both issuer inputs are checked before either
copy. Values are copied into immutable references and reread for verification; existing values
must match, never be overwritten. Client metadata is bounded and validated before any client
secret creation, then reference-only metadata is published after the copies succeed.

An interruption may leave some destination secrets without a completed migration. Those copies
are retained: replay compares them with the source and resumes rather than rotating credentials.
A difference is an explicit repair error. Legacy issuer files remain protected inactive recovery
material; they are not runtime fallback. Client migration atomically supersedes the inline registry
only after custody succeeds. Signing-key bytes, client IDs, grants, refresh families, and vault
content are unchanged. Never delete old keys to bypass migration. An absent source is not invented
during migration; fresh normal startup may initialize a genuinely new credential without replacement.
Explicit migration may restore a lost credential from its retained protected legacy file only when
the source matches the established fingerprint. A changed source is rejected before either copy.
Migration without a source cannot invent a lost bound credential. Preview never creates receipts;
apply records/verifies them before publishing migrated client metadata.

## Limits and diagnostics

Opaque values are non-empty and at most 64 KiB; logical names are at most 128 ASCII bytes and reject
path syntax and Windows device names. Client registry metadata remains bounded to 1 MiB.
Secret values, issuer configuration/runtime objects, and registered-client debug views are redacted.
JSON decode diagnostics omit source values. The authorized OAuth registration response may return a
new confidential client secret once, as required by the protocol; management reports do not.

Tests cover fresh/restarted custody, instance isolation, public/confidential clients, dry-run
immutability, running-instance refusal, partial-copy replay, mismatched existing credentials,
reference substitution, missing values, retained legacy keys, output redaction, and live
foreground/resident grant parity. The protected-store foundation has Windows cross-target
checks; full daemon cross-compilation requires the Windows C compiler used by native dependencies.
Native Windows execution remains a CI/platform test, not a claim made by a Linux run.
