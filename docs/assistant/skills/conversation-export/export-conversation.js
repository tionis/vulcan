#!/usr/bin/env -S vulcan run --script
function main(input) {
  const title = text(input.title).trim();
  const transcript = text(input.transcript);
  if (!title) {
    throw new Error("title is required");
  }
  if (!transcript.trim()) {
    throw new Error("transcript is required");
  }

  const source = text(input.source).trim() || "manual";
  const day = normalizeDate(input.date) || new Date().toISOString().slice(0, 10);
  const folder = normalizeFolder(input.target_folder) || "AI/Conversations";
  const messages = parseMessages(transcript);
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

function parseMessages(transcript) {
  const jsonMessages = parseJsonMessages(transcript);
  if (jsonMessages.length > 0) {
    return jsonMessages;
  }
  const blocks = parseRoleBlocks(transcript);
  if (blocks.length > 0) {
    return blocks;
  }
  return [{ role: "user", content: transcript.trim() }];
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
  if (Array.isArray(value.messages)) {
    return messagesFromJsonValue(value.messages);
  }
  const role = normalizeRole(value.role || value.author || value.type);
  const content = value.content || value.text || value.message || value.output;
  if (!content) {
    return [];
  }
  return [{ role, content: normalizeContent(content) }];
}

function parseRoleBlocks(transcript) {
  const messages = [];
  let current = null;

  for (const line of transcript.split(/\r?\n/)) {
    const match = line.match(/^\s*(user|human|assistant|system|tool)\s*:\s*(.*)$/i);
    if (match) {
      if (current && current.content.trim()) {
        messages.push({ role: current.role, content: current.content.trim() });
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
    messages.push({ role: current.role, content: current.content.trim() });
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
    for (const line of message.content.split(/\r?\n/)) {
      lines.push(line ? `> ${line}` : ">");
    }
    blocks.push(lines.join("\n"));
  }
  return `${blocks.join("\n\n")}\n`;
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
