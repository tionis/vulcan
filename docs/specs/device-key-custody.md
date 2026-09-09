# Device Key Custody and Secret Stores

Status: accepted design for Roadmap 12.16; not yet implemented.

This specification extends `device-identity.md`. It changes where the private half of a device
identity may be kept, not how a device is identified or trusted.

## Decision summary

Vulcan separates four concerns:

1. **Identity:** the canonical SSH public key and its derived `vdev1_...` ID.
2. **Custody:** the provider currently capable of using the corresponding private key.
3. **Usage:** an explicit operation such as device proof, SSH authentication, or signing.
4. **Trust and authority:** a later policy deciding whether a key may perform that usage.

Moving the same private key between custody providers preserves the device ID. Generating or selecting
a different key creates a different device. Storage migration is therefore not key rotation and never
creates a predecessor/successor relationship.

There is no universally reliable cross-platform keychain. Vulcan keeps its protected OpenSSH file as
the portable, unattended baseline and adds explicit platform or agent providers. It never silently
selects a provider, copies a key to a fallback, or changes providers because the configured one is
locked or unavailable.

## Architecture

The synchronous application layer owns two related interfaces:

- `SecretStore` stores bounded opaque bytes under a non-secret logical name. It supports capability
  inspection, create-without-replacement, get, and delete. It can later serve connector/API secrets,
  but adopting it does not automatically migrate current environment-variable credentials.
- `KeyProvider` exposes one exact public key, availability/capabilities, and bounded signing
  without returning private bytes to callers. File and system-secret adapters may internally decode an
  exportable OpenSSH key for the shortest possible time; agent or hardware adapters can sign without
  making key bytes available to Vulcan. `DeviceKeyProvider` is the adapter binding this interface to
  `identity.json`; it does not duplicate provider storage or signing code. Only trusted typed application
  operations may call it; transports, JS, and plugins never receive a raw signer.

The higher-level `KeyManager` in `key-management.md` owns cryptographic key identity, algorithm and
usage validation, principal bindings, lifecycle, and registry integration. `SecretStore` deliberately
does not grow those semantics: it remains reusable for non-key credentials as well as key blobs.

Provider I/O remains synchronous and bounded so direct CLI, daemon blocking workers, tests, and future
transports share one implementation. Platform adapters live below `vulcan-app`; neither `vulcan-core`
nor `vulcan-sync` learns keychain APIs, prompts, environment sockets, or private-key formats. The sync
engine receives only the already resolved device ID and explicit transport/signing capability it needs.

Secret values use a dedicated non-serializable `SecretBytes` type with redacted `Debug`/error behavior,
a fixed maximum size, and zeroization on drop. This reduces exposure but does not promise that an OS,
debugger, swap, core dump, or compromised process cannot observe memory. Private bytes are never cloned
into reports, durable journals, configuration, tracing fields, notifications, panic text, or test
snapshots.

Every provider reports a structured state rather than flattening failures into “missing”:

```text
available | locked | prompt_required | unavailable | missing | denied | invalid | unsupported | unknown
```

It also reports relevant capabilities such as `persistent`, `device_local`, `exportable`,
`non_exporting_signer`, `interactive`, `unattended`, `ssh_agent`, and supported key/signature algorithms.
Human and JSON output may name the provider and state but never its secret value or an unsafe raw locator.
Capabilities are `supported`, `unsupported`, or `unknown`, scoped to an operation and execution context.
An unclassified provider refusal stays unknown/denied; it must not be guessed to mean missing or locked.
`unattended` means prompting can be suppressed by contract, not that a previous test happened to succeed.
Successful interactive proof is never evidence of unattended availability.

## Identity manifest

The identity manifest remains public device-local metadata. Its closed version-1 shape is:

```json
{
  "version": 1,
  "scheme": "ssh-ed25519-sha256-v1",
  "device_id": "vdev1_<52 lowercase RFC 4648 Base32 characters>",
  "public_key": "ssh-ed25519 <canonical base64>",
  "key_provider": "file_v1"
}
```

`key_provider` is a closed versioned discriminator, not an arbitrary command, URI, object path, or
filename. Version 1 reserves `file_v1`, `system_secret_v1`, and `ssh_agent_v1`; implementations reject
unknown values. The concrete provider can find its item deterministically from the complete device ID:

- protected file: the fixed sibling `device/id_ed25519`;
- Apple: service `dev.tionis.vulcan.device-key`, account `<device-id>`;
- Windows: generic credential target `dev.tionis.vulcan/device-key/<device-id>`;
- Secret Service: exact non-secret attributes `application=dev.tionis.vulcan`, `kind=device-key`, and
  `device-id=<device-id>`; and
- SSH agent: exact canonical public-key blob among the identities offered by the configured agent.

Vulcan does not persist native keychain object paths or agent indexes. Secret Service explicitly
recommends attribute lookup rather than recording object paths, and agent ordering is not identity.
Provider lookup must reject duplicates rather than choosing the first match.

The manifest and `.pub` projection remain in Vulcan's user-data directory and are sufficient for
read-only identity, ref, and provenance operations. When custody is unavailable, the key-derived ID is
still a valid local label, but it remains no more remotely authenticated than before. An operation that
actually requires signing or device-key SSH authentication must obtain the configured provider and
fails closed if it cannot.

## Providers and platform policy

### Protected OpenSSH file (`file_v1`)

This remains the default because it is portable, works in headless and service environments, can be
used directly by OpenSSH, and does not turn vault preservation into a desktop-session dependency. The
unencrypted private key is protected by owner-only directory/file permissions and symlink/reparse-point
checks. Full-disk encryption and operating-system account security remain important. “File available”
must never be rendered as “hardware protected” or “keychain protected.”

### Native system secret store (`system_secret_v1`)

This stores the bounded unencrypted OpenSSH private-key serialization as an opaque binary secret. It is
encrypted/protected by the platform facility but remains exportable into the Vulcan process when used;
it is not a non-exportable hardware key.

- **macOS:** use `SecItem`, prefer the data-protection keychain when the signed CLI/LaunchAgent packaging
  can access the same item, and set `kSecAttrSynchronizable=false`. Choose the most restrictive
  accessibility class that passes the supported user-session daemon conformance tests; never enable
  iCloud synchronization, because copying the key would copy the device identity.
- **Windows:** use a `CRED_TYPE_GENERIC` item with `CRED_PERSIST_LOCAL_MACHINE`, which is available to
  subsequent logon sessions of the same user on the same computer but not that user on other computers.
  Do not use enterprise/roaming persistence. If Credential Manager cannot meet packaging or size
  requirements, a separately versioned user-scoped DPAPI encrypted-file provider may be designed; it is
  not silently substituted under `system_secret_v1`, and machine-wide DPAPI is forbidden.
- **Linux desktop:** use the default Secret Service collection and exact lookup attributes, not a
  recorded D-Bus object path. Collections may be locked and unlocking may require a prompt, so this
  provider is opt-in and cannot claim unattended availability until tested from the installed systemd
  user service environment. A missing D-Bus session or service is `unavailable`, not an empty store.
- **Headless Linux and Android/Termux:** retain `file_v1` initially. Linux kernel keyrings are not a
  durable reboot-persistent replacement. A Termux process cannot claim access to another Android app's
  Keystore identity; a future signed companion may expose a narrow signing service, but that is a new
  provider and requires Android lifecycle, authorization, and real-device conformance work.

The build should use explicit, reviewed platform adapters or narrowly selected `keyring-core` stores,
not a runtime “native” backend whose meaning can change after a dependency update. Dependencies and
features are pinned and compiled only on their target platforms. Provider names in the manifest keep
their semantics across upgrades.

### SSH agent (`ssh_agent_v1`)

An SSH agent supplies operations, not durable storage. V1 supports ordinary Ed25519 signing keys through
an explicitly configured/inherited socket and exact canonical public-key selection. Generic external
agents are interactive-only: adoption, verification, migration proof, and signing require an explicit
`--interactive` TTY invocation. A background call refuses before sending any signing request. Public
enumeration may report the exact key as present, with signing availability unknown.

The standard agent protocol exposes public keys/comments, not complete confirmation, lifetime, lock,
or destination constraints; it has no standard per-request no-prompt flag. The agent can prompt in its
own process even if the caller disables askpass. A timeout bounds waiting but cannot prevent a prompt.
Do not infer unattended suitability from enumeration, a successful past signature, or a socket path.
Future unattended agent support requires a separately versioned enforceable provider contract.

Destination-constrained authentication keys may reject arbitrary signing challenges while remaining
usable for an allowed SSH destination. They are unsupported as v1 device signers; do not remove their
constraints or call them corrupt. A future authentication-only adapter must verify the actual bound
authentication operation separately. Hardware/security-key algorithms are also deferred from v1.

The daemon never searches shell state or reads sockets from vault config. Generic agent signing is not
available to daemon, cron, MCP, JS, or companion calls. This restriction applies to Vulcan-managed key
operations; ordinary existing user-managed Git credentials remain outside this provider integration.

## Provider selection and operation policy

`file_v1` is the default at device initialization. A user may explicitly request another available
provider. There is no durable `auto` provider: dry-run may recommend a provider, but the manifest always
records the concrete selected value.

Provider availability is checked per required use:

- ordinary sync ref naming, capture, comparison, recovery inspection, and unsigned provenance need the
  public manifest only and continue when private custody is locked or unavailable;
- key-storage verification and migration require a proof-of-possession signature;
- future signed statements require signing capability in their exact SSHSIG namespace; and
- future Git-over-SSH use requires an explicit transport binding supported by that provider.

This prevents a locked desktop keychain from disabling lossless local capture when the key is not part
of the active transport. If a required device-key operation fails, sync captures and anchors local bytes
before returning when repository access permits, does not update canonical refs or apply remote files,
and emits the ordinary actionable daemon attention event. It never retries through another provider.

Interactive unlock or confirmation is opt-in and TTY-only. A non-interactive operation invokes a
provider only when it can enforce prompt suppression; otherwise it fails before the operation with
`prompt_required` or `unsupported`. Native providers must use their no-UI facility or refuse. Status,
doctor, and dry-run never sign, decrypt secrets, or unlock; availability may be unknown. Actual I/O is
bounded, and cancellation stops waiting without claiming an already issued signature can be recalled.

## Storage migration transaction

`vulcan device key migrate --to <file|system|ssh-agent> [--interactive] [--dry-run]` changes custody of
the exact current key; it cannot accept a different public key. Migration is serialized and uses a
durable, secret-free state journal outside the vault and cache:

1. Resolve the source manifest and provider; verify the manifest's public key and device ID. Plan every
   known dependent usage and execution context, including daemon and configured SSH transport. List
   external/unmanaged uses as unknown; never claim complete knowledge of external consumers.
2. Probe the destination without prompting and refuse an existing ambiguous or different item. Block
   a switch that cannot preserve a configured usage. The operator must separately disable or reconfigure
   that usage first; migration does not rewrite transport/daemon configuration. A system-secret signing
   proof cannot replace a Git `IdentityFile`; a generic agent cannot replace required unattended signing. Unknown required
   capabilities block the switch until explicitly verified in the affected context.
3. Copy/import the key when export is supported, or require that an agent/non-exporting destination
   already offer the exact public key.
4. Under the permitted prompt policy, sign a fresh domain-separated random challenge through the
   destination and verify it against the manifest public key. Verify the planned dependent operations
   in their actual execution contexts before switching. If a dependency cannot be tested safely,
   require prior explicit disabling of that usage; interactive proof alone never satisfies a daemon check.
5. Recheck manifest and dependency revisions under the shared operation lock, then atomically change
   only `key_provider` and reread through the ordinary loader. A changed dependency invalidates the plan;
   do not mix configuration edits into the custody transaction.
6. Repeat proof through the now-active provider.
7. Retain the source as `active_with_retained_source`. Migration never deletes private material.
   The old copy is inactive and cannot be used as fallback. Cleanup is a separate explicit operation.

Every phase is idempotent after interruption. Before the manifest switch, the source remains active;
after it, the destination remains active. If post-switch proof fails, retain both copies and report
repair-required; do not silently roll back. Cleanup re-verifies the active provider, required usages,
and source identity under the same lock. It rejects an active source, a superseded migration, or an item
recreated at the same location. Journals bind a local custody generation as well as key and provider;
every Vulcan custody mutation advances that generation. If an external store cannot establish exact
item identity safely, automated cleanup is unsupported. V1 cleanup cannot delete the last known durable
copy; agent availability is not evidence of durable backing. Deletion failure preserves the journal.

Migration out of a non-exporting provider is possible only when the exact same key is already available
through the destination. Otherwise the user must create a new device identity; Vulcan cannot manufacture
or claim continuity. No private key is printed to stdout. A later explicit recovery export, if added,
writes only to a new owner-only file after warning that importing it elsewhere clones the same device.

## CLI and configuration surface

- `vulcan device key status` reads the manifest and probes the configured provider, reporting custody
  state, capabilities, unattended suitability, duplicate migration residue, and exact repair guidance.
- `vulcan device key verify [--interactive]` signs and locally verifies a fresh challenge without remote
  or vault mutation.
- `vulcan device key migrate --to ... --dry-run` inspects public source/destination metadata and reports
  unknown checks, prompt requirements, dependent usages, key import, manifest switch, retained source,
  daemon restart, or SSH reconfiguration. It does not sign or decrypt. Applying uses the journal above.
- `vulcan device key cleanup --migration <id> [--dry-run]` removes only a verified inactive duplicate
  left by a completed migration.
- `vulcan sync doctor` and `vulcan daemon status` include the provider's public state and whether current
  configured sync usages can run unattended. They do not unlock, prompt, migrate, or enumerate unrelated
  keychain entries.

Future generic secret references use structured device-local configuration such as a source kind plus a
bounded logical name. Environment-variable references remain supported. Secret values never appear in
TOML, and Vulcan does not silently move existing environment values into a store. Listing may expose
logical names and provider state only to an authorized local/admin surface, never secret contents.

Optional synced secrets follow `key-management.md` and Roadmap 12.18: ciphertext may sync, while a
dedicated age private identity is shared out of band and imported into local custody. This does not
change device-key identity, enable SSH-agent decryption, or enroll the age key in the signing registry.
Protected local identity files suffice initially; `SecretStore` can hold the same identity bytes later.

## Trust, signing, and Git boundaries

Custody says where a key can be used; it does not establish that anyone should trust it. A successful
local challenge proves current provider control of the private key, not device ownership, authorization,
remote registration, or the integrity of a remote ref.

System-secret providers contain exportable OpenSSH bytes and do not automatically integrate with the
system OpenSSH client. Future Git authentication must explicitly choose one of these reviewed paths:

- `file_v1`: an exact `IdentityFile` invocation with `IdentitiesOnly=yes` and existing host verification;
- a future authentication agent adapter: the configured socket and exact public identity with
  operation-specific constraints and an enforceable prompt policy; or
- a future Vulcan-owned agent bridge that exposes only the configured key's signing operation.

Vulcan never writes a broad user SSH configuration, disables host-key checking, forwards an agent by
default, exports a native-store key to a temporary file, or falls back to another Git credential. Forge
key registration remains an explicit provider/account operation with separate authorization.

Signed device statements and semantic Git signing later use typed `KeyManager` operations with fixed
payloads and namespaces; transports cannot call a provider directly. Deterministic internal sync
commits stay unsigned. Registry history verification uses the public state in `key-management.md`,
not the custody manifest. Future registry consumers use the last accepted state while offline and
enforce revocations as accepted updates arrive; their adapters must preserve this explicit contract.

`key-management.md` defines that accepted public registry and its local inventory boundary. Custody
providers implement operations for the key manager; they never decide principals, usages, trust,
retirement, compromise, administrator authority, or registry acceptance themselves.

## Tests and conformance

All providers share a fake-store/provider conformance suite covering create-without-replacement,
duplicate lookup, bounded values, redaction, unavailable/locked/prompt states, exact-key selection,
challenge verification, cancellation boundaries, and deletion failures. Migration tests inject a crash
after every phase and prove exactly one manifest-selected provider remains authoritative.
Test refusal before any generic-agent background sign request, hidden confirmation constraints,
authentication-only keys, ambiguous agent failures, and dry-run without signing or secret reads. Test
interactive success with daemon failure, file-to-system migration with a dependent `IdentityFile`,
post-switch proof failure, retained source, repeated migrations, stale cleanup generations, and refusal
to delete the last durable copy. Keep migration tests independent from optional native-provider tests.

Platform gates use native runners and the installed daemon/service context:

- macOS: unsigned/development and release-signed CLI/LaunchAgent access, non-synchronizable attributes,
  locked-session behavior, upgrade/reinstall, and no unexpected prompt;
- Windows: current-user Generic Credential isolation, local-machine persistence without roaming,
  service/logon-task access, duplicate targets, and uninstall preservation;
- Linux: GNOME Keyring and KWallet Secret Service implementations, locked/default collections, absent
  D-Bus/service, systemd user environment, prompt refusal, and explicit headless file selection; and
- Android/Termux: explicit unsupported system-store result and unchanged protected-file behavior until a
  real app bridge exists.

Tests scan every human/JSON/error/debug/log/notification path for private material and provider raw
locators. Release qualification includes reboot/login, daemon restart, offline sync capture, interrupted
migration recovery, and downgrade behavior that fails safely without deleting or rewriting either key
copy when the older binary cannot understand the active provider.

## Primary platform references

- [Apple Keychain Services](https://developer.apple.com/documentation/security/keychain-services/)
  and [TN3137 on macOS keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
- [Apple `kSecAttrSynchronizable`](https://developer.apple.com/documentation/security/ksecattrsynchronizable)
- [Microsoft `CredWrite`](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew)
  and [`CREDENTIAL` persistence](https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw)
- [Freedesktop Secret Service API](https://specifications.freedesktop.org/secret-service/latest-single/)
- [Android Keystore](https://developer.android.com/privacy-and-security/keystore)
- [OpenSSH `ssh-add`](https://man.openbsd.org/ssh-add)
- [SSH agent protocol, RFC 9987](https://www.rfc-editor.org/rfc/rfc9987.html)
- [`ssh-key` format/crypto support](https://docs.rs/ssh-key/latest/ssh_key/)
- [`keyring-core` ecosystem guidance](https://docs.rs/keyring/latest/keyring/)
