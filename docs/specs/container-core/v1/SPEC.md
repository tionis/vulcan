# Container Core version 1

Container Core defines the rules shared by Vulcan's immutable exchange formats: serializations, member paths, archive safety, manifests and member declarations, digests, logical identity, declared sources, source locators, provenance, lineage, and extensions. Container Core is a specification layer, not a file format. No producer writes a "core" file. A **format** uses Container Core by naming its manifest, suffixes, member roles, and fixed members, and it may add stricter rules.

Formats that use Container Core v1:

| Format | Manifest | Directory / ZIP suffix | Purpose |
|---|---|---|---|
| [MDAF v1](../../mdaf/v1/SPEC.md) | `info.json` | `.mdaf` / `.mdaf` | One document extracted from source media, with its evidence |
| [Markdown Wiki Package v2](../../wiki-package/v2/SPEC.md) | `wiki.json` | `.wikibundle` / `.wikipack` | A wiki tree built from sources, with provenance and optional knowledge |

[Wiki Package v1](../../wiki-package/v1/SPEC.md) predates Container Core and keeps its own identity rule.

The machine-readable shared definitions are [`defs.schema.json`](defs.schema.json) (digests, producer, sources, byte spans, locators, and selectors) and [`provenance.schema.json`](provenance.schema.json). Each format schema embeds exact copies of the shared `$defs` it uses so that it stays self-contained. A copy must not diverge from this directory. Vulcan's test suite enforces this.

## Serializations

A package is either a directory or a ZIP file, and the name of each ends in the format's suffix. Both serializations expose the same regular members at the same paths and have the same logical identity. ZIP timestamps, compression method, entry order, and permissions do not affect identity. Empty directories carry no meaning.

## Member paths

Member paths use UTF-8, `/` separators, Unicode NFC, and relative POSIX syntax. The following are invalid:

- empty components, `.`, and `..`;
- absolute paths, backslashes, and control characters;
- Windows drive or UNC prefixes;
- duplicates, including case-fold-equivalent and normalization-equivalent duplicates.

Directory readers reject symbolic links and special files. Readers never write outside an isolated staging area, and they stream member bytes through bounded readers.

## Archive safety

Readers reject:

- encrypted ZIP members and symbolic-link modes;
- more than 100,000 entries;
- any member larger than 2 GiB;
- more than 8 GiB of total expanded content;
- any member whose expansion ratio is above 1,000:1.

Formats set tighter limits for their control members. JSON control documents are limited to 32 MiB, and JSON Lines sidecars to 512 MiB. Readers check declared sizes before extraction and fail when observed bytes differ.

## Manifest and members

Each format has exactly one root JSON manifest at a fixed path. The manifest declares every other regular file exactly once, and an undeclared, missing, or unknown member is invalid. Each declaration has at least these fields:

- `path`;
- `role`, from the format's role table;
- `media_type`;
- `size`, the exact byte count;
- `digest`, the canonical digest of the exact bytes;
- `created_by`, the ID of the provenance activity that emitted the member.

Optional fields are `schema` (a public schema identifier) and `namespace`, which extension members require. A format assigns each role either a fixed root path or a directory prefix. A role never depends on a filename, extension namespace, producer, or tool.

Manifests reject unknown fields. Producer-specific data belongs in an `extension` member below `extensions/<namespace>/`. `<namespace>` is a reverse-domain authority such as `dev.tionis.renwiki`, and it equals the member's `namespace`. Consumers accept and ignore extension members they don't understand, after validating their path, size, and digest.

## Digests

Container Core v1 uses 256-bit BLAKE3 as its canonical digest. A digest is written as `blake3:` followed by 64 lowercase hexadecimal characters. A source may also list `alternate_digests` in the form `<algorithm>:<lowercase hex>` so that upstream identities are preserved. Alternate digests never replace or weaken the canonical digest.

## Logical identity

To compute the logical identity of a package:

1. For every regular member, **including the manifest**, compute its canonical digest.
2. Sort the records by normalized UTF-8 path bytes.
3. Serialize each record as compact JSON with keys in exactly this order, followed by LF:

   ```json
   {"path":"wiki.json","size":123,"digest":"blake3:<64 lowercase hex>"}
   ```

   Strings use JSON escaping without ASCII-only conversion.
4. The identity is the canonical digest of the concatenated records.

Because the manifest is included, any change to title, producer, sources, or lineage creates a new identity. Each format ships a test vector.

## Sources

The manifest's `sources` array declares the inputs a package cites. Each source has:

- a stable package-local `id`;
- a `media_type`;
- a canonical `digest`;
- optionally `alternate_digests`, a display `name`, and an `embedded_path`.

`embedded_path` names a member with the `source` role whose digest equals the source digest. Source IDs are unique within a package. Portable fields must not contain credentials, signed URLs, authorization headers, or absolute local paths.

## Byte spans

A span `{start, end}` is a zero-based, half-open range of UTF-8 bytes in a named text member. Both endpoints must fall on character boundaries, and `start < end`. A span addresses the member's declared bytes. It still refers to those bytes after a consumer materializes and edits the text.

## Locators and selectors

A **locator** names exactly one declared source and holds an ordered list of selectors. An empty list selects the complete source. Otherwise the selectors are conjunctive refinements: an `interval` for page 12 followed by a `rectangle` selects that rectangle on page 12. The order records the natural outside-in refinement and is preserved, but it does not change the selected segment. Half-open ranges include their start and exclude their end.

The normalized selectors are:

- `interval`: a non-empty numeric range in an open unit such as `byte`, `unicode-scalar`, `line`, `page`, `slide`, `frame`, `sample`, `millisecond`, or `second`. The optional `origin`, `label_start`, and `label_end` preserve numbering conventions without changing the numeric range.
- `rectangle`: `x`, `y`, `width`, and `height` in an open unit. `pixel`, `percent`, and `normalized` have their ordinary top-left-origin meaning. Percent values are bounded by 100 and normalized values by 1.
- `polygon`: three or more points that enclose a non-zero area, for regions a rectangle cannot represent accurately.
- `grid`: zero-based, half-open row and column ranges, with an optional sheet name.
- `text-quote`: exact text with optional prefix and suffix context.
- `fragment`: a media-defined fragment value with an optional public `conforms_to` identifier. Examples are HTML IDs, EPUB CFI, program labels, and node IDs.
- `extension`: opaque JSON under a `reverse.domain/name` namespace, for locators that the normalized selectors cannot represent without loss.

All numbers are finite. Consumers validate selectors but never infer their meaning from a source media type. A new source format therefore needs neither a new Container Core version nor a consumer code branch: it uses the closest lossless normalized selectors and keeps any richer native locator in an `extension` selector.

## Provenance

`provenance.json` conforms to [`provenance.schema.json`](provenance.schema.json). It is an acyclic activity graph. Each activity records:

- `id` and `kind`;
- optional timestamps;
- every directly participating tool (name and version, plus a revision when available);
- models, with provider, identifier, returned identifier, and revision or checksum when exposed;
- `inputs`, which are member paths or `source:<id>`;
- `outputs`, which are member paths;
- `depends_on`;
- sanitized, output-affecting `parameters` and their `parameters_digest`.

Every declared member, including `provenance.json` itself, is listed in the outputs of the activity named by its `created_by`.

`parameters_digest` is the canonical digest of `parameters` serialized as compact UTF-8 JSON. Object keys are sorted recursively by Unicode scalar value, arrays keep their order, strings use normal JSON escaping without ASCII-only conversion, numbers use their JSON lexical form, and no whitespace or trailing newline is emitted. Producers should use strings for values whose numeric lexical form is significant.

A mutable or unresolved model alias is marked `mutable-alias` or `unavailable` and produces a reproducibility warning. It is never replaced with invented provenance. Transport secrets, credentials, signed URLs, and private endpoint topology are forbidden. A redaction record names the member, field location, and reason, plus the original field digest when that is safe to compute.

## Lineage

`derived_from` lists the logical identities of immutable parent packages. It is lineage only, not an instruction to fetch anything. A derivative is self-contained. Improving a package creates a new package with its own provenance and never edits the old one in place.

## Consumer rules

Consumers:

- validate schemas, member declarations, digests, spans, locators, and provenance before any mutation;
- never select behavior from a producer, tool, model, filename, or extension namespace;
- keep a package as external evidence after import. Imported files become canonical vault content, and the package never becomes authoritative cache state.

Structural validity does not establish that a claim is true, that a mapping is semantically correct, or that a hierarchy is useful.
