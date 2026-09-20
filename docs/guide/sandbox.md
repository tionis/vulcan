# JavaScript Sandbox and Permissions

Vulcan runs JavaScript in an embedded QuickJS runtime. A sandbox level controls which runtime
capabilities are exposed, while the active permission profile controls which resources those
capabilities may access. Trust checks provide a separate execution gate for vault-owned code such
as plugins and skill commands.

All applicable boundaries must allow an operation. Selecting a broader sandbox never widens the
active permission profile or bypasses trust.

## Sandbox levels

| Level | Read vault | Write vault | Network | Host processes | Runtime limits |
| --- | --- | --- | --- | --- | --- |
| `strict` (default) | Yes | No | No | No | Enforced |
| `fs` | Yes | Yes | No | No | Enforced |
| `net` | Yes | Yes | Yes | No | Enforced |
| `none` | Yes | Yes | Yes | Permission-gated | Disabled |

The levels are cumulative for vault and network capabilities:

- `strict` supports pure computation and read helpers backed by indexed vault data.
- `fs` adds write APIs such as `vault.create()`, `vault.patch()`, and `vault.transaction()`.
- `net` adds `web.search()` and `web.fetch()`.
- `none` exposes the full runtime surface, including `host`, and removes the QuickJS memory,
  stack, and execution-time limits.

Use the narrowest level that satisfies the workflow. `none` is not merely another convenience
tier: code can execute local programs and is no longer protected by the normal runtime limits.

## Permissions still apply

Sandbox selection and permission profiles answer different questions:

- The sandbox decides whether an API namespace or operation can be used at all.
- The permission profile restricts readable and writable paths, network origins, process
  execution, shell execution, and other application capabilities.
- Trust decides whether vault-owned executable code may start.

For example, `--sandbox net` exposes `web.fetch()`, but the request still needs a network rule that
allows its origin. Likewise, `--sandbox fs` exposes write helpers, but the target must be writable
under the effective profile.

Inspect the active runtime permissions from JavaScript with `vulcan.permissions()`. Inspect CLI
configuration with `vulcan config show` and use `--permissions <profile>` when selecting an
explicit profile.

## Host execution

Host process APIs are deliberately more restrictive:

- `host.exec(argv, opts?)` requires `--sandbox none` and explicit `execute` permission.
- `host.shell(command, opts?)` requires `--sandbox none` plus explicit `execute` and `shell`
  permission.

Prefer `host.exec()` because its argument vector avoids shell parsing. Use `host.shell()` only when
shell syntax is essential. Both APIs return bounded process reports, but the surrounding `none`
sandbox does not enforce the usual QuickJS runtime limits.

Projected skill command tools may declare only `strict`, `fs`, or `net`; `sandbox = none` is
invalid for that surface. Prefer built-in APIs or compose another permissioned tool instead of
giving an exposed skill command unrestricted runtime access.

## Resource limits

For `strict`, `fs`, and `net`, QuickJS enforces configured memory, stack, and execution-time
limits. The defaults live in `.vulcan/config.toml`:

```toml
[js_runtime]
memory_limit_mb = 64
stack_limit_kb = 256
default_timeout_seconds = 30
default_sandbox = "strict"
scripts_folder = ".vulcan/scripts"
```

`vulcan run --timeout <duration>` may set a per-run timeout. Selecting `none` disables the runtime
memory, stack, and execution-time limits rather than granting an infinite safe budget.

Dangerous user-visible globals such as `eval` and `Function` are removed from the exposed runtime
surface. That hardening does not make untrusted `none` code safe.

## Runtime surfaces

The sandbox model is shared by:

- `vulcan run` scripts and the interactive REPL
- DataviewJS evaluation
- Templater-compatible JavaScript
- JavaScript plugins and lifecycle hooks
- skill command scripts, subject to their stricter declaration rules

These surfaces may inherit sandbox, permission, and trust settings from their command or
configuration. Do not assume that a script which works under an interactive `vulcan run` invocation
has the same authority when projected as a tool.

JavaScript support is compiled behind the default `js_runtime` feature. Builds created with
`--no-default-features` do not provide JS-dependent execution, DataviewJS, Templater JavaScript, or
the JS web APIs.

## Recommended escalation path

Start with `strict`, then move only as far as required:

```bash
vulcan run --sandbox strict inspect.js
vulcan run --sandbox fs update-summary.js
vulcan run --sandbox net import-release-notes.js
vulcan run --sandbox none local-process.js --permissions trusted-local
```

Before escalating, prefer a dedicated Vulcan command or namespace over generic network or process
access. Keep write operations transactional, restrict allowed paths and origins in the profile, and
avoid running vault-owned code until the vault is trusted.

See also `vulcan help scripting`, `vulcan help js.contract`, `vulcan help js.host`, and
`vulcan help permissions`.
