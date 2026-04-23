function firstParagraph(markdown) {
  return String(markdown)
    .split(/\r?\n\r?\n/)
    .map((chunk) => chunk.trim())
    .find((chunk) => chunk && !chunk.startsWith("---") && !chunk.startsWith("#")) ?? "";
}

function main(input, ctx) {
  const note = vault.note(input.note);
  const preview = firstParagraph(note.content).slice(0, 240);

  return {
    result: {
      note: note.file.path,
      title: note.file.name,
      preview,
      tool: ctx.tool.name,
    },
    text: preview ? `${note.file.path}: ${preview}` : `Summarized ${note.file.path}`,
  };
}
