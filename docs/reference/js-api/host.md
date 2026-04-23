# JS Host Execution

The `host` namespace exposes permission-gated local process execution inside the shared Vulcan JS
runtime.

Use it when a script, plugin, or custom tool must call a local program and there is no safer
built-in `vault.*`, `web.*`, or `tools.*` API for the job.

Available entrypoints:

- `host.exec(argv, opts?)`
  - requires `execute`
  - takes an explicit argument vector
  - does not invoke a shell parser
- `host.shell(command, opts?)`
  - requires `shell`
  - runs through the configured shell
  - higher risk because shell parsing and expansion are involved

Current runtime surfaces:

- `vulcan run`
- the JS REPL (`vulcan run`)
- `vulcan dataview query-js`
- custom tool entrypoints
- plugin hooks executed through the shared JS runtime

`opts` shape for both calls:

- `cwd?: string`
  - working directory relative to the current script file when one exists, otherwise the vault root
- `env?: object`
  - environment overrides for the child process
  - `null` removes one inherited variable
- `timeout_ms?: number`
  - tighter subprocess timeout cap
- `max_output_bytes?: number`
  - tighter per-stream stdout/stderr capture cap

Return shape:

- `success: boolean`
- `exit_code: number | null`
- `stdout: string`
- `stderr: string`
- `truncated_stdout: boolean`
- `truncated_stderr: boolean`
- `timed_out: boolean`
- `duration_ms: number`
- `invocation`
  - `{ kind: "exec", argv: [...] }`
  - or `{ kind: "shell", command: "...", shell: "..." }`

Execution rules:

- non-zero exit codes are returned in the report; they do not throw by themselves
- spawn failures, invalid arguments, permission denials, and invalid `cwd` values throw
- subprocess timeouts inherit the surrounding JS runtime timeout unless `timeout_ms` asks for a
  tighter limit
- output capture defaults to bounded stdout/stderr buffers and can be tightened per call

Preferred pattern:

```js
function main(_input, ctx) {
  return host.exec(
    ["git", "status", "--short"],
    {
      cwd: ".",
      env: {
        GIT_ASKPASS: null,
        API_TOKEN: ctx.secrets?.get?.("git_token") ?? null,
      },
    }
  );
}
```

Why `host.exec()` is preferred:

- argument vectors avoid quoting bugs
- logs and audits are easier to reason about
- permissioned tools should minimize shell expansion and injection surface

Use `host.shell()` only when shell syntax is genuinely required, for example pipelines or compound
shell conditionals that would otherwise need a wrapper script.

See also:

- `help js.tools`
- `help tool`
- `help automation-surfaces`
