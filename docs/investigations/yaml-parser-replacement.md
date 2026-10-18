# Replacing `serde_yaml` (Roadmap 6.8)

Date: 2026-10-18. Outcome: `serde_yaml` 0.9.34 replaced by `yaml_serde` 0.10.7.

## Why

`serde_yaml` is archived upstream and marked deprecated. It depends on `unsafe-libyaml`, which
RUSTSEC-2024-0449 flags as unmaintained, so `deny.toml` carried an exception for it.

## Candidates

| Crate | Latest release | Parser underneath | Notes |
|---|---|---|---|
| `yaml_serde` | 0.10.7, 2026-08-18 | `libyaml-rs` 0.3.0 (YAML org) | Fork published by the YAML organization; same API as `serde_yaml` 0.9; MSRV 1.82 |
| `serde_norway` | 0.9.42, 2024-12-21 | `unsafe-libyaml-norway` | No release in almost two years |
| `serde_yaml_ng` | 0.10.0, 2024-05-26 | `unsafe-libyaml` | Keeps the flagged dependency |
| `serde-saphyr` | 2.0.0, 2026-10-13 | `saphyr` (pure Rust) | Different parser and no drop-in `Value`; error text and edge cases would change |

`yaml_serde` is the only maintained drop-in. `libyaml-rs` is still libyaml translated to unsafe
Rust, so the change is about maintenance, not memory safety; a pure-Rust parser would be a larger,
behavior-changing migration to decide separately.

Vulcan had no YAML wrapper layer: about 350 direct uses of `serde_yaml::Value`, `Mapping`,
`from_str`, and `to_string` across `vulcan-core`, `vulcan-app`, and `vulcan-sync`, and
`ParsedDocument::frontmatter` exposes the `Value` type. The swap renames the crate path and
changes nothing else in the code.

## Equivalence

A throwaway harness, kept outside the repository, parsed every YAML document with both crates and
compared the `Value` (`Debug`), the re-emitted text (`to_string`), the `serde_json::Value`
deserialization, and the error text when both rejected a document. The corpus:

- all `.base`, `.yaml`, and `.yml` files and every Markdown frontmatter block in this repository
  (fixture vaults, references, docs);
- every Markdown frontmatter block and YAML file in the five wikis registered on the development
  machine (about 4,200 notes);
- 32 hand-written edge cases: YAML 1.1 booleans, octal and hex, special floats, null forms,
  timestamps, anchors, aliases, merge keys, custom and core tags, duplicate keys, block scalars,
  quoting, complex keys, top-level scalars and sequences, multi-document input, recursive
  aliases, `!!binary`, integers past `u64`, directives, comments, long lines, and malformed input.

Result: 7,701 documents (7,667 parsed by both, 34 rejected by both), no differences in values,
emitted text, JSON, or error text.

`properties::tests::frontmatter_yaml_semantics_are_pinned` pins the behavior frontmatter
properties rely on (canonical JSON and inferred types). It passes against both crates; it was run
once with the workspace dependency pointed back at `serde_yaml` 0.9.34. Notable shared behavior it
records: merge keys stay a literal `<<` key, a leading-zero number such as `017` stays text,
custom tags reduce to their value, and `.inf` becomes JSON `null`.

## Consequences

- `deny.toml` no longer ignores RUSTSEC-2024-0449; `cargo deny check` passes.
- Library consumers that named `serde_yaml::Value` (for example through
  `ParsedDocument::frontmatter`) now get `yaml_serde::Value`. No parsing behavior changed, so
  the parser version is not bumped.
