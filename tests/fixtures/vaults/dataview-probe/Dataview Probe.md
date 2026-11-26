---
aliases: [Probe Alias]
tags: [probe/front]
date: 2026-01-15
datetime: 2026-01-15T10:30
number: 42
float: 3.5
flag: true
text: plain text
link: "[[People/Alice]]"
links: ["[[People/Alice]]", "[[Other Note]]"]
list: [a, b]
nested:
  inner: value
---
# Dataview Probe

This note checks DataviewJS compatibility. Open it in Obsidian with Dataview enabled and copy the
JSON block it renders; Vulcan's tests render the same note and compare the two.

Inline:: inline value
Inline Date:: 2026-02-03
Inline Link:: [[People/Alice]]
Inline Number:: 7
Inline Duration:: 2 hours
Mixed [bracket:: one] and (paren:: two) fields. #probe/body

## Lists

- Plain item [cost:: -10.64 / 12] [groups:: a, b]
- Item with link [[Other Note]] and [up:: [[People/Alice]]]
	- Child item [child:: yes]
- status:: listed
- Repeated [rep:: 1] [rep:: 2]

## Tasks

- [ ] Open task [due:: 2026-04-01] #probe/task
- [x] Done task ✅ 2026-04-02
- [/] In progress [priority:: high]
	- [ ] Nested task 📅 2026-05-01

## Probe

```dataviewjs
(async () => {
  const kind = (v) => {
    if (v === null) return "null";
    if (v === undefined) return "undefined";
    if (typeof v === "function") return "function";
    if (typeof v !== "object") return typeof v;
    if (typeof v.where === "function" && typeof v.array === "function") return "DataArray";
    if (Array.isArray(v)) return "Array";
    if (typeof v.obsidianLink === "function" || (typeof v.path === "string" && "embed" in v)) return "Link";
    if (typeof v.toFormat === "function" && typeof v.toMillis === "function") return "DateTime";
    if (typeof v.shiftTo === "function" || v.isLuxonDuration === true) return "Duration";
    return "object";
  };
  const plain = (v, depth = 0) => {
    const k = kind(v);
    if (k === "DataArray" || k === "Array") {
      const values = k === "DataArray" ? v.array() : v;
      return depth > 3 ? `${k}(${values.length})` : values.map((x) => plain(x, depth + 1));
    }
    if (k === "Link") return `Link(${v.path}|${v.display ?? ""}|${v.subpath ?? ""}|${v.embed}|${v.type})`;
    if (k === "DateTime") return `DateTime(${v.toFormat("yyyy-MM-dd'T'HH:mm")})`;
    if (k === "Duration") return `Duration(${v.toISO ? v.toISO() : String(v)})`;
    if (k === "object") {
      if (depth > 3) return "object";
      const out = {};
      for (const key of Object.keys(v).sort()) out[key] = plain(v[key], depth + 1);
      return out;
    }
    return v;
  };
  // Key -> kind of value, for objects whose values vary between machines.
  const shape = (o) => {
    const out = {};
    for (const key of Object.keys(o ?? {}).sort()) {
      const value = o[key];
      const k = kind(value);
      const first = k === "DataArray" ? value.array()[0] : k === "Array" ? value[0] : undefined;
      out[key] = first === undefined ? k : `${k} of ${kind(first)}`;
    }
    return out;
  };
  const attempt = async (f) => {
    try {
      return plain(await f());
    } catch (error) {
      return `THROWS: ${error && error.message ? error.message : error}`;
    }
  };
  const fieldsOf = (item) => {
    const out = {};
    const builtin = new Set(["symbol", "link", "section", "text", "tags", "line", "lineCount", "list",
      "outlinks", "path", "children", "task", "annotated", "position", "subtasks", "real", "header",
      "parent", "blockId", "status", "checked", "completed", "fullyCompleted", "visual", "created",
      "due", "completion", "start", "scheduled"]);
    for (const key of Object.keys(item).sort()) {
      if (!builtin.has(key)) out[key] = plain(item[key]);
    }
    return out;
  };

  const page = dv.current();
  const file = page.file;
  const result = {};

  result.pageKeys = shape(Object.fromEntries(Object.entries(page).filter(([key]) => key !== "file")));
  result.pageValues = await attempt(() =>
    Object.fromEntries(Object.entries(page).filter(([key]) => key !== "file")));
  result.fileKeys = shape(file);
  result.fileValues = await attempt(() => ({
    name: file.name, folder: file.folder, path: file.path, ext: file.ext,
    link: file.link, aliases: file.aliases, tags: file.tags, etags: file.etags,
    outlinks: file.outlinks, inlinks: file.inlinks, day: file.day, frontmatter: file.frontmatter,
  }));
  result.lists = file.lists.array().map((item) => ({
    text: item.text,
    keys: Object.keys(item).sort(),
    fields: fieldsOf(item),
    parent: kind(item.parent),
    children: item.children.length,
    task: item.task,
  }));
  result.tasks = file.tasks.array().map((task) => ({
    text: task.text,
    status: task.status,
    checked: task.checked,
    completed: task.completed,
    due: plain(task.due),
    completion: plain(task.completion),
    fields: fieldsOf(task),
    subtasks: (task.subtasks ?? task.children ?? []).length,
  }));

  const dvMembers = ["pagePaths", "page", "pages", "current", "query", "tryQuery", "queryMarkdown",
    "tryQueryMarkdown", "evaluate", "tryEvaluate", "execute", "executeJs", "array", "isArray",
    "isDataArray", "fileLink", "sectionLink", "blockLink", "date", "duration", "parse", "literal",
    "clone", "compare", "equal", "el", "header", "paragraph", "span", "view", "list", "table",
    "taskList", "markdownTable", "markdownList", "markdownTaskList", "io", "func", "luxon",
    "container", "currentFilePath"];
  result.dvMembers = Object.fromEntries(dvMembers.map((name) => [name, kind(dv[name])]));
  const arrayMembers = ["where", "filter", "map", "flatMap", "mutate", "limit", "slice", "concat",
    "indexOf", "find", "findIndex", "includes", "join", "sort", "sortInPlace", "groupBy", "groupIn",
    "distinct", "every", "some", "none", "first", "last", "to", "into", "expand", "forEach", "sum",
    "avg", "min", "max", "array", "length", "values"];
  const sample = dv.pages('"People"');
  result.dataArrayMembers = Object.fromEntries(arrayMembers.map((name) => [name, kind(sample[name])]));

  result.behaviour = {
    dateParse: await attempt(() => dv.date("2026-01-02")),
    dateShorthand: await attempt(() => kind(dv.date("today"))),
    duration: await attempt(() => dv.duration("3 days")),
    compare: await attempt(() => [dv.compare(1, 2), dv.compare("b", "a"), dv.equal([1], [1])]),
    parseLink: await attempt(() => dv.parse("[[People/Alice]]")),
    parseDate: await attempt(() => dv.parse("2026-01-02")),
    fileLink: await attempt(() => dv.fileLink("People/Alice")),
    sectionLink: await attempt(() => dv.sectionLink("People/Alice", "Heading")),
    pageByPath: await attempt(() => dv.page("People/Alice").role),
    pageByLink: await attempt(() => dv.page(page.link).role),
    pagePaths: await attempt(() => dv.pagePaths('"People"')),
    swizzle: await attempt(() => dv.pages('"People"').file.name),
    whereStatus: await attempt(() => dv.pages().where((p) => p.status).status),
    groupOrder: await attempt(() => dv.array(["b", "a", "b"]).groupBy((x) => x).map((g) => g.key)),
    sortKeyArgs: await attempt(() => dv.array([2, 1]).sort((value, index) => typeof index)),
    toString: await attempt(() => String(dv.array([1, 2]))),
    tryEvaluate: await attempt(() => dv.tryEvaluate("1 + 2")),
    evaluateLink: await attempt(() => dv.tryEvaluate("this.link", { link: "[[People/Alice]]" })),
    queryIsPromise: await attempt(() => typeof dv.query("LIST").then),
    queryResult: await attempt(async () => (await dv.query('LIST FROM "People"')).successful),
    queryMarkdown: await attempt(async () => (await dv.queryMarkdown('LIST FROM "People"')).value),
    markdownTable: await attempt(() => dv.markdownTable(["a"], [[1]])),
    markdownList: await attempt(() => dv.markdownList([1, 2])),
    ioNormalize: await attempt(() => dv.io.normalize("Alice")),
    ioLoadIsPromise: await attempt(() => typeof dv.io.load("People/Alice.md").then),
    funcCount: await attempt(() => Object.keys(dv.func).length > 30),
    luxon: await attempt(() => kind(dv.luxon.DateTime.fromISO("2026-01-02"))),
    inlinkSource: await attempt(() => file.inlinks.map((link) => link.path)),
    frontmatterDateKind: await attempt(() => kind(page.date)),
    frontmatterLinkKind: await attempt(() => kind(page.link)),
    inlineDurationKind: await attempt(() => kind(page["inline-duration"])),
    mtimeKind: await attempt(() => kind(file.mtime)),
    ctimeKind: await attempt(() => kind(file.ctime)),
    cdayKind: await attempt(() => kind(file.cday)),
    sizeKind: await attempt(() => typeof file.size),
    starred: await attempt(() => file.starred),
  };

  // Escape "::" so Dataview's reading-view inline-field renderer leaves the copied JSON intact.
  const json = JSON.stringify(result, null, 1).replace(/::/g, "\\u003a\\u003a");
  dv.paragraph("```json\n" + json + "\n```");
})();
```
