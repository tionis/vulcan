type VulcanToolCall = {
  name: string;
  arguments?: Record<string, unknown>;
};

type VulcanToolResult = {
  ok: boolean;
  stdout?: string;
  stderr?: string;
  status?: number | null;
  error?: string;
};

const vaultRoot = process.env.VULCAN_VAULT_ROOT ?? process.cwd();
const permissionProfile = process.env.VULCAN_ASSISTANT_PERMISSIONS ?? "readonly";
const toolPacks = (process.env.VULCAN_ASSISTANT_TOOL_PACKS ?? "notes-read,search,status")
  .split(",")
  .map((pack) => pack.trim())
  .filter(Boolean);

function blocksBuiltInTool(call: VulcanToolCall): string | null {
  if (permissionProfile !== "readonly") {
    return null;
  }
  if (["bash", "edit", "write"].includes(call.name)) {
    return `built-in ${call.name} is blocked by Vulcan permission profile ${permissionProfile}`;
  }
  return null;
}

async function runVulcanTool(call: VulcanToolCall): Promise<VulcanToolResult> {
  const blocked = blocksBuiltInTool(call);
  if (blocked) {
    return { ok: false, error: blocked };
  }

  const command = call.arguments?.command;
  if (!Array.isArray(command) || !command.every((arg) => typeof arg === "string")) {
    return {
      ok: false,
      error: "expected tool arguments.command to be a string array",
    };
  }

  const childProcess = await import("node:child_process");
  const result = childProcess.spawnSync("vulcan", [
    "--vault",
    vaultRoot,
    "--permissions",
    permissionProfile,
    "--output",
    "json",
    ...command,
  ], {
    cwd: vaultRoot,
    encoding: "utf8",
    maxBuffer: 10 * 1024 * 1024,
  });

  return {
    ok: result.status === 0,
    stdout: result.stdout,
    stderr: result.stderr,
    status: result.status,
    error: result.error?.message,
  };
}

export default function registerVulcanExtension(pi: any) {
  const summary = `Vulcan vault root: ${vaultRoot}
Vulcan permission profile: ${permissionProfile}
Vulcan tool packs: ${toolPacks.join(", ")}`;

  pi?.hooks?.before_agent_start?.tap?.("vulcan-context", (context: any) => {
    context.systemPrompt = `${context.systemPrompt ?? ""}\n\n${summary}`;
    return context;
  });

  pi?.hooks?.tool_call?.tap?.("vulcan-policy", (call: VulcanToolCall) => {
    const blocked = blocksBuiltInTool(call);
    if (blocked) {
      throw new Error(blocked);
    }
    return call;
  });

  pi?.registerTool?.({
    name: "vulcan_cli",
    description: "Run a Vulcan CLI command through the active permission profile.",
    inputSchema: {
      type: "object",
      properties: {
        command: {
          type: "array",
          items: { type: "string" },
          description: "Vulcan command arguments, excluding global --vault/--output/--permissions.",
        },
      },
      required: ["command"],
      additionalProperties: false,
    },
    execute: runVulcanTool,
  });
}
