# Knowledge version 1

Knowledge v1 is an immutable, source-neutral snapshot of **entities** and **claims** together with the evidence that each cites. It is the shared intermediate record between whatever analysis produced it and the wiki pages rendered from it. The same contract works for a game script, a rulebook, a transcript, or any other source whose segments a [Container Core](../../container-core/v1/SPEC.md) locator can address.

A snapshot is not a working ledger. Issued jobs, alternative results, selection, and editorial overlays belong to the producer's own mutable state. A snapshot records only the result the producer chose to publish, and states the review status of each record.

A snapshot does not stand on its own. The **host** package provides the declared source table that evidence locators resolve against and, optionally, the notes that records may point to. [Markdown Wiki Package v2](../../wiki-package/v2/SPEC.md) hosts a snapshot as `knowledge.jsonl`.

## Serialization

A snapshot is UTF-8 [JSON Lines](https://jsonlines.org/): one compact JSON object per line. Every line, including the last, ends with LF, and blank lines are invalid. Each record conforms to [`knowledge-record.schema.json`](knowledge-record.schema.json). Records appear in this order:

1. exactly one header, `{"record":"header","format":"dev.tionis.knowledge","version":1}`;
2. every entity record, sorted by strictly increasing `id` (UTF-8 bytes);
3. every claim record, sorted by strictly increasing `id`.

IDs match `^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$` and are unique across all entities and claims. Producers should derive them from their own durable identities so that successive snapshots keep them stable. The fixed order makes snapshots diffable and lets a consumer validate references in one pass.

## Evidence

An evidence item is `{"role": ..., "locator": ...}`. The locator is a Container Core locator into a source declared by the host. The role is one of:

- `support`: the cited segment supports the record;
- `contradict`: the cited segment conflicts with the record;
- `context`: the cited segment is needed to interpret the record.

A citation proves only that the producer tied the record to the cited segment. It does not prove that the segment entails the claim, that the speaker is reliable, or that the claim holds everywhere.

## Entities

```json
{"record":"entity","id":"alice","name":"Alice","type":"character",
 "aliases":["Al"],"summary":"A lifelong resident of the harbor.",
 "note":"content/Characters/Alice.md","review":"accepted",
 "evidence":[{"role":"support","locator":{"source_id":"script","selectors":[{"type":"interval","unit":"line","start":2,"end":3}]}}]}
```

| Field | Meaning |
|---|---|
| `id`, `name` | Required. `name` is the display name. |
| `type` | Required. One of `character`, `faction`, `location`, `organization`, `event`, `item`, `technology`, `species`, `culture`, `religion`, `concept`, `rule`, or `other`, or a namespaced `reverse.domain/name` type. |
| `aliases` | Unique alternative names. |
| `summary` | Optional one-paragraph producer summary. |
| `note` | The host note that describes this entity, if any. |
| `review` | Required. `accepted` (a human accepted the record) or `unreviewed`. Rejected records are not published. |
| `evidence` | Required, at least one item. |

## Claims

```json
{"record":"claim","id":"alice-lives-at-harbor","subject":"alice","predicate":"lives-in",
 "object":{"kind":"entity","entity":"harbor"},"assertion":"attributed","attributed_to":"alice",
 "polarity":"positive","scope":[{"dimension":"route","value":"common"}],"review":"unreviewed",
 "confidence":0.8,"evidence":[...],"notes":[{"note":"content/Characters/Alice.md","document":{"start":30,"end":75}}]}
```

| Field | Meaning |
|---|---|
| `subject` | Required. An entity `id`. |
| `predicate` | Required. An open vocabulary; producers should use stable kebab-case verbs. |
| `object` | Required. Either `{"kind":"entity","entity":<id>}` or `{"kind":"text","value":<text>}`. |
| `assertion` | Required. How the source presents the claim: `attributed` (a named speaker says it), `narrated` (the source states it directly), `inferred` (derived from several segments), `interpretation` (an analytic reading), or `uncertain`. |
| `attributed_to` | Required when `assertion` is `attributed`, and forbidden otherwise. An entity `id`. |
| `polarity` | Required. `positive` or `negative`. |
| `scope` | Optional unique qualifiers `{dimension, value}` that limit where the claim holds. The dimension is `route`, `timeline`, `edition`, or `condition`, or a namespaced `reverse.domain/name`. An unscoped claim makes no scope assertion. |
| `review` | Required, as for entities. |
| `confidence` | Optional producer confidence in `[0, 1]`. It is not a correctness measure. |
| `evidence` | Required, with at least one `support` item. |
| `notes` | Optional host notes that render the claim, each with an optional byte span in that note. |

## Validation

A consumer validates:

- each record against the schema;
- that the header comes first, entities precede claims, and IDs are sorted and unique;
- that every entity reference (subject, entity object, speaker) resolves;
- attribution consistency;
- that each claim has at least one `support` item;
- that each locator names a source declared by the host and uses valid selectors;
- that each note reference names a declared host note and that its span is a valid UTF-8 range in that note.

A standalone snapshot, one with no host notes, must not contain note references.

## Consumer behavior

Consumers preserve the review status, confidence, and assertion mode exactly and never promote `unreviewed` records. They never treat a successful validation as semantic verification. Rendering pages from a snapshot is the producer's responsibility. A consumer may index a hosted snapshot, for example to query claims by entity or source segment, but that index is derived state that can be rebuilt from the package.
