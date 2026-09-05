# MDAF wiki pipeline repair validation

2026-09-05. Tested locally against three distinct private Mistral OCR book
artifacts, with a fourth supplied filename duplicating an existing identity.
No private documents, native responses, or artifact identities are fixtures.

## Contract changes

The producer publishes a separately versioned alternative outline rather than
rewriting primary Markdown heading levels. Vulcan remains provider-neutral:
outline authority is explicit; authored heading and block anchors remain the
link targets; unsupported bounded outline sections fail before mutation.
The manifest title becomes root metadata without shifting source offsets.
Validation warnings and large root remainders are visible in import reports.

Page references may annotate plain prose as well as existing links. Every note
overlapping the target mapping is considered, not just the note at its first
byte. Only unique destinations and safe, unique plain-text placements become
links. Code, overlapping annotations, ambiguous pages, and repeated output
text stay unchanged with diagnostics. A page-level mapping cannot reliably
identify a sub-page topic: finer splitting intentionally resolves fewer links.

These are v1 semantic clarifications, not a format-version or schema change.
The bundled artifact-import skill now describes authority selection, hierarchy
depth, root-remainder review, and conservative reference handling.

## Whole-book checks

| Corpus | Original default notes | Repaired chapter notes | Repaired topic notes | Assets |
| --- | ---: | ---: | ---: | ---: |
| 98-page book | 184 | 7 | 325 | 32 |
| 502-page book | 869 | 17 | 1,915 | 330 |
| 257-page book | 712 | 9 | 771 | 55 |

All counts include root. Repaired outlines have an explicit front-matter note.
All six applied imports had zero source bytes stranded in root, complete
non-overlapping source-span coverage, byte-identical assets, and valid generated
navigation/citation file targets. Chapter-level plain citations resolved
0/152/15 times; topic-level citations resolved 0/16/2 times. Duplicate authored
headings and mutable-model warnings remain visible rather than being hidden.

Blobforge's opt-in Mistral wiki-v4 recipe (normalization wiki-v3) replays native
evidence without provider calls, retains original Markdown/native bytes, and
records its hierarchy evidence. It remains heuristic: missing chapter evidence
is not invented, and source labels/ranges/external citations without sufficient
evidence are not converted to links. Earlier recipes and hosted defaults remain
unchanged. Keep original MDAFs archived outside imported note trees.

## Verification

Synthetic Rust regressions cover alternative-outline anchor preservation,
section-boundary rejection, root titles, surfaced validation warnings, protected
prose, repeated placements, and coarse source mappings. CLI preview/apply
assertions cover root titles. Run the standard fmt/clippy/workspace test gates.

On this host, `/bin/sh -lc` fails because the user login profile evaluates Bash
activation syntax. The complete workspace suite passes with only the child
namespace's profile isolated, using:

```sh
bwrap --bind / / --dev /dev --ro-bind /path/to/empty-profile /home/eric/.profile \
  cargo test --workspace
```

This is a test-environment workaround, not an application change or a proposal
to alter the user's profile. No paid OCR calls, remote publication, or deployment
were performed.
