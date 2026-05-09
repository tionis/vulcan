#!/usr/bin/env -S vulcan skill exec
function main(input) {
  const title = text(input.title).trim();
  const transcript = text(input.transcript);
  if (!title) {
    throw new Error("title is required");
  }
  if (!hasConversationInput(input, transcript)) {
    throw new Error("transcript, messages, or turns is required");
  }

  const source = text(input.source).trim() || "manual";
  const day = normalizeDate(input.date) || new Date().toISOString().slice(0, 10);
  const folder = normalizeFolder(input.target_folder) || "AI/Conversations";
  const messages = parseConversationInput(input, transcript);
  const markdown = renderConversation(title, messages);
  const path = `${folder}/${day}-${slugify(title)}.md`;
  const roles = Array.from(new Set(messages.map((message) => message.role)));

  if (input.dry_run === true) {
    return {
      path,
      title,
      source,
      message_count: messages.length,
      roles,
      markdown,
    };
  }

  const createdPath = createUnique(path, markdown, {
    type: "conversation",
    title,
    source,
    imported: new Date().toISOString(),
    message_count: messages.length,
    roles,
  });

  return {
    path: createdPath,
    title,
    source,
    message_count: messages.length,
    roles,
  };
}

function text(value) {
  return value == null ? "" : String(value);
}

function hasConversationInput(input, transcript) {
  return transcript.trim() || Array.isArray(input.messages) || Array.isArray(input.turns);
}

function normalizeDate(value) {
  const raw = text(value).trim();
  if (!raw) {
    return "";
  }
  const match = raw.match(/^(\d{4}-\d{2}-\d{2})/);
  if (!match) {
    throw new Error("date must begin with YYYY-MM-DD");
  }
  return match[1];
}

function normalizeFolder(value) {
  const folder = text(value)
    .replace(/\\/g, "/")
    .replace(/^\/+|\/+$/g, "")
    .replace(/\/+/g, "/");
  if (!folder) {
    return "";
  }
  if (folder.split("/").some((part) => part === "." || part === "..")) {
    throw new Error("target_folder must stay inside the vault");
  }
  return folder;
}

function slugify(value) {
  const slug = text(value)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "conversation";
}

function parseConversationInput(input, transcript) {
  const structured = messagesFromJsonValue(input.turns || input.messages || []);
  if (structured.length > 0) {
    return structured;
  }
  if (transcript.trim()) {
    return parseMessages(transcript);
  }
  return [];
}

function parseMessages(transcript) {
  const jsonMessages = parseJsonMessages(transcript);
  if (jsonMessages.length > 0) {
    return jsonMessages;
  }
  const blocks = parseRoleBlocks(transcript);
  if (blocks.length > 0) {
    return blocks;
  }
  return [{ role: "user", parts: [{ type: "text", content: transcript.trim() }] }];
}

function parseJsonMessages(transcript) {
  const trimmed = transcript.trim();
  if (!trimmed) {
    return [];
  }
  try {
    const value = JSON.parse(trimmed);
    return messagesFromJsonValue(value);
  } catch (_error) {
    // Fall through to JSONL and role-prefixed text.
  }

  const messages = [];
  for (const line of trimmed.split(/\r?\n/)) {
    const candidate = line.trim();
    if (!candidate) {
      continue;
    }
    try {
      const value = JSON.parse(candidate);
      messages.push(...messagesFromJsonValue(value));
    } catch (_error) {
      return [];
    }
  }
  return messages;
}

function messagesFromJsonValue(value) {
  if (Array.isArray(value)) {
    return value.flatMap(messagesFromJsonValue);
  }
  if (!value || typeof value !== "object") {
    return [];
  }
  if (Array.isArray(value.turns)) {
    return messagesFromJsonValue(value.turns);
  }
  if (Array.isArray(value.messages)) {
    return messagesFromJsonValue(value.messages);
  }
  const role = normalizeRole(value.role || value.author || value.type);
  const parts = normalizeParts(value);
  if (parts.length === 0) {
    return [];
  }
  return [{ role, parts }];
}

function normalizeParts(value) {
  const parts = [];
  appendContentParts(parts, value.content || value.text || value.message || value.output);
  appendThinkingPart(parts, value.thinking || value.reasoning || value.thoughts);
  appendToolUseParts(parts, value.tool_uses || value.toolUses || value.tools);
  appendToolResultParts(parts, value.tool_results || value.toolResults);
  return parts;
}

function appendContentParts(parts, content) {
  if (content == null) {
    return;
  }
  if (Array.isArray(content)) {
    for (const item of content) {
      if (item && typeof item === "object") {
        appendTypedPart(parts, item);
      } else {
        appendTextPart(parts, "text", item);
      }
    }
    return;
  }
  appendTextPart(parts, "text", content);
}

function appendTypedPart(parts, item) {
  const type = text(item.type).toLowerCase();
  if (["thinking", "reasoning"].includes(type)) {
    appendTextPart(parts, "thinking", item.text || item.content || item.thinking);
    return;
  }
  if (["tool_use", "tool-call", "tool_call"].includes(type)) {
    appendToolUseParts(parts, [item]);
    return;
  }
  if (["tool_result", "tool-output", "tool_output"].includes(type)) {
    appendToolResultParts(parts, [item]);
    return;
  }
  appendTextPart(parts, "text", item.text || item.content || item.value || item);
}

function appendThinkingPart(parts, thinking) {
  appendTextPart(parts, "thinking", thinking);
}

function appendToolUseParts(parts, toolUses) {
  if (!Array.isArray(toolUses)) {
    return;
  }
  for (const toolUse of toolUses) {
    if (!toolUse || typeof toolUse !== "object") {
      continue;
    }
    parts.push({
      type: "tool_use",
      id: text(toolUse.id || toolUse.call_id || toolUse.tool_call_id).trim(),
      name: text(toolUse.name || toolUse.tool || toolUse.function || toolUse.function_name).trim() || "tool",
      input: toolUse.input ?? toolUse.arguments ?? toolUse.args ?? toolUse.params,
      output: toolUse.output ?? toolUse.result,
      error: toolUse.error,
    });
  }
}

function appendToolResultParts(parts, toolResults) {
  if (!Array.isArray(toolResults)) {
    return;
  }
  for (const toolResult of toolResults) {
    if (!toolResult || typeof toolResult !== "object") {
      continue;
    }
    parts.push({
      type: "tool_result",
      id: text(toolResult.id || toolResult.call_id || toolResult.tool_call_id).trim(),
      name: text(toolResult.name || toolResult.tool || toolResult.function || toolResult.function_name).trim() || "tool",
      output: toolResult.output ?? toolResult.result ?? toolResult.content,
      error: toolResult.error,
    });
  }
}

function appendTextPart(parts, type, value) {
  const content = normalizeContent(value);
  if (content) {
    parts.push({ type, content });
  }
}

function parseRoleBlocks(transcript) {
  const messages = [];
  let current = null;

  for (const line of transcript.split(/\r?\n/)) {
    const match = line.match(/^\s*(user|human|assistant|system|tool)\s*:\s*(.*)$/i);
    if (match) {
      if (current && current.content.trim()) {
        messages.push({ role: current.role, parts: [{ type: "text", content: current.content.trim() }] });
      }
      current = {
        role: normalizeRole(match[1]),
        content: match[2] || "",
      };
      continue;
    }
    if (current) {
      current.content += `${current.content ? "\n" : ""}${line}`;
    }
  }

  if (current && current.content.trim()) {
    messages.push({ role: current.role, parts: [{ type: "text", content: current.content.trim() }] });
  }
  return messages;
}

function normalizeRole(value) {
  const role = text(value).toLowerCase();
  if (role === "human") {
    return "user";
  }
  if (["assistant", "system", "tool", "user"].includes(role)) {
    return role;
  }
  return "assistant";
}

function normalizeContent(value) {
  if (typeof value === "string") {
    return value.trim();
  }
  if (Array.isArray(value)) {
    return value.map(normalizeContent).filter(Boolean).join("\n\n");
  }
  return JSON.stringify(value, null, 2);
}

function renderConversation(title, messages) {
  const blocks = [`# ${title}`];
  for (const message of messages) {
    const lines = [`> [!${message.role}]+ ${labelForRole(message.role)}`];
    appendMessageParts(lines, message.parts || [{ type: "text", content: message.content || "" }]);
    blocks.push(lines.join("\n"));
  }
  return `${blocks.join("\n\n")}\n`;
}

function appendMessageParts(lines, parts) {
  for (const part of parts) {
    if (part.type === "thinking") {
      appendNestedTextCallout(lines, "thinking", "Thinking", part.content);
    } else if (part.type === "tool_use") {
      appendToolCallout(lines, "Tool use", part);
    } else if (part.type === "tool_result") {
      appendToolCallout(lines, "Tool result", part);
    } else {
      appendQuotedLines(lines, part.content);
    }
  }
}

function appendQuotedLines(lines, content) {
  for (const line of text(content).split(/\r?\n/)) {
    lines.push(line ? `> ${line}` : ">");
  }
}

function appendNestedTextCallout(lines, kind, label, content) {
  lines.push(`> > [!${kind}]- ${label}`);
  for (const line of text(content).split(/\r?\n/)) {
    lines.push(line ? `> > ${line}` : "> >");
  }
}

function appendToolCallout(lines, label, part) {
  const suffix = part.id ? ` (${part.id})` : "";
  lines.push(`> > [!tool]- ${label}: ${part.name}${suffix}`);
  if (part.input !== undefined) {
    appendNestedJsonBlock(lines, "input", part.input);
  }
  if (part.output !== undefined) {
    appendNestedJsonBlock(lines, "output", part.output);
  }
  if (part.error !== undefined) {
    appendNestedJsonBlock(lines, "error", part.error);
  }
}

function appendNestedJsonBlock(lines, label, value) {
  lines.push(`> > ${label}:`);
  lines.push("> > ```json");
  for (const line of JSON.stringify(value, null, 2).split(/\r?\n/)) {
    lines.push(`> > ${line}`);
  }
  lines.push("> > ```");
}

function labelForRole(role) {
  return role.charAt(0).toUpperCase() + role.slice(1);
}

function createUnique(basePath, markdown, frontmatter) {
  const dot = basePath.toLowerCase().endsWith(".md") ? basePath.length - 3 : basePath.length;
  const stem = basePath.slice(0, dot);
  const suffix = basePath.slice(dot);
  for (let attempt = 1; attempt <= 100; attempt += 1) {
    const path = attempt === 1 ? basePath : `${stem}-${attempt}${suffix}`;
    try {
      vault.create(path, { content: markdown, frontmatter });
      return path;
    } catch (error) {
      if (!String(error).includes("destination note already exists")) {
        throw error;
      }
    }
  }
  throw new Error("could not choose a unique export path");
}
