# Git Realtime Events profile

**Status:** Draft 0.1  
**Profile identifier:** `https://tionis.dev/spec/git-realtime/1`  
**Depends on:** [Event Relay Protocol](event-relay-protocol.md), CloudEvents 1.0

This profile describes small realtime notifications for Git reference changes. It is forge-neutral and can be produced by a native Git receive hook, a Forgejo/Gitea/GitHub/GitLab webhook adapter, or another source that can prove an accepted reference update.

Events are hints. Git object and reference state obtained from an authenticated Git remote remains authoritative.

## 1. Repository identity

Each event source MUST assign a stable opaque repository URI. It MUST NOT depend on one clone URL remaining permanent and SHOULD NOT reveal an owner or repository name when the channel is private.

```text
urn:git-repository:01K00000000000000000000000
```

Clone URLs are optional descriptor hints and are never repository credentials. A consumer MUST explicitly bind a repository identity to a configured Git remote; it MUST NOT guess equivalence from repository names or URL suffixes.

## 2. Event types

Version 1 defines two event types:

- `dev.tionis.git.refs.updated.v1`
- `dev.tionis.git.ref.state.v1`

The reverse-DNS names are provisional while the specification lives in this repository. A future transfer to a neutral standards home requires a compatibility alias rather than silently changing already emitted types.

### 2.1 `refs.updated`

This immutable event represents one observed accepted receive result. One event can contain multiple reference updates; adapters MUST NOT split an atomic multi-ref update into events that imply independent acceptance. Grouping alone does not claim that the original push requested or received atomic treatment.

Required CloudEvent attributes:

- `source`: stable opaque repository URI;
- `id`: unique event ID within that source;
- `type`: `dev.tionis.git.refs.updated.v1`;
- `time`: time at which the source accepted or observed the transaction;
- `datacontenttype`: `application/json`.

Required data fields:

```json
{
  "object_format": "sha1",
  "atomic": false,
  "updates": [
    {
      "ref": "refs/heads/main",
      "before": "0123456789abcdef0123456789abcdef01234567",
      "after": "89abcdef0123456789abcdef0123456789abcdef",
      "forced": false
    }
  ]
}
```

Rules:

- `object_format` MUST be `sha1`, `sha256`, or a later value registered by this profile.
- `atomic` is optional and MUST be present with value `true` only when the producer can prove the source accepted the ref updates atomically. `false` or absence makes no atomicity claim.
- OIDs MUST be complete lowercase hexadecimal object names of the declared format.
- `ref` MUST be a complete valid Git reference name, not a short branch name.
- `before` MUST be `null` for creation and otherwise an OID.
- `after` MUST be `null` for deletion and otherwise an OID.
- Both OIDs MUST NOT be `null` in one update.
- A ref MUST occur at most once in one event.
- `forced` is optional. If an adapter cannot prove ancestry, it MUST omit the field rather than guess.
- Array order has no semantic meaning. Producers SHOULD sort updates by ref for stable diagnostics.
- Commit messages, diffs, changed paths, pusher identity, and repository credentials MUST NOT be included in version 1.

### 2.2 `ref.state`

This event advertises the latest observed value of one ref and is designed for `latest_by_subject` retention.

Required CloudEvent attributes are the same as `refs.updated`, except:

- `type` is `dev.tionis.git.ref.state.v1`;
- `subject` is the complete ref name.

Data shape:

```json
{
  "object_format": "sha1",
  "ref": "refs/heads/main",
  "oid": "89abcdef0123456789abcdef0123456789abcdef"
}
```

`oid` is `null` when the ref is absent. The `data.ref` value MUST exactly equal the CloudEvent `subject`. A producer SHOULD emit both `refs.updated` and current `ref.state` events when the relay offers both bounded-log and latest-state channels.

## 3. Producer rules

A producer MUST emit only after the source reports that the reference transaction was accepted. A webhook adapter MUST authenticate the webhook before parsing or normalizing it. If the forge reports a transaction before the new ref is immediately fetchable, the adapter or descriptor SHOULD advertise an expected visibility delay; consumers still handle this through bounded fetch retry.

Adapters SHOULD preserve the source webhook delivery ID as part of deterministic deduplication, but the normalized CloudEvent ID must remain unique under its `source`. Repeated delivery of the same authenticated webhook SHOULD produce the same `(source, id)`.

Source-specific information that cannot be represented faithfully MUST be omitted or placed in a separately versioned extension. An adapter MUST NOT claim atomicity, ancestry, or object visibility it cannot establish.

## 4. Consumer rules

A consumer:

1. validates the Event Relay and CloudEvents envelopes;
2. validates this profile and the expected channel/repository binding;
3. filters on explicitly configured full ref names or ref patterns;
4. treats `(source, id)` duplicates as harmless;
5. schedules or coalesces a reconciliation with the bound Git remote;
6. fetches the ref through ordinary authenticated Git transport;
7. verifies actual remote state instead of applying an advertised OID directly.

Unknown event types MUST NOT trigger Git mutation. A malformed or mismatched event MUST be diagnosed and acknowledged or quarantined according to client policy so it cannot cause an infinite redelivery loop.

Polling remains a correctness fallback. Missing, late, reordered, or malicious notifications can change latency but must not change the eventual result of Git reconciliation.

## 5. Discovery and subscription

Version 1 requires explicit subscription-bundle import. This permits a standalone relay to work with any forge that can send a webhook, without modifying the forge's Git server.

The onboarding flow is:

1. create a Git event channel at a relay;
2. register the relay's source-specific webhook URL and webhook secret at the forge;
3. export a read-only subscription bundle;
4. import and explicitly bind that bundle to a local Git remote and selected refs.

An HTTPS source descriptor MAY be linked from a forge UI or repository metadata. A future Git protocol v2 extension may advertise a descriptor URL because protocol v2 permits unknown capabilities, but this draft does not reserve or require a Git capability name before there is an interoperable server implementation.

## 6. Authorization

Public repositories MAY expose public notifications. Private repository notification channels SHOULD use per-subscriber `bearer_capability` credentials from the Event Relay Protocol.

Read authority covers only event metadata. It does not grant Git object access, and successful relay authentication MUST NOT be treated as Git authentication. Conversely, possession of Git clone credentials does not automatically authorize relay access unless a future delegated authorization profile explicitly defines that exchange.

Webhook signing secrets and publish credentials are never included in source descriptors or subscription bundles. Subscriber credentials never authorize webhook ingress.

## 7. Forge adapter conformance

Each adapter must have fixtures for:

- branch creation, fast-forward, force update, and deletion;
- tag creation and deletion;
- a push updating multiple refs;
- duplicate webhook delivery;
- invalid source authentication;
- missing optional ancestry information;
- repository rename without repository-identity change;
- event normalization without pusher or credential leakage.

The initial reference adapter targets Forgejo. GitHub, GitLab, and Gitea adapters are additive and must emit the same profile events for equivalent source operations.

## 8. Vulcan binding

Vulcan binds the event's repository `source` plus selected refs to one or more registered Git wikis. A valid matching event produces the existing `RemoteNotification` job trigger. It does not call Git directly from the event client and does not create a notification-specific merge path.

The ordinary supervisor coalesces duplicate triggers and runs the same finite capture, fetch, deterministic merge, conflict preservation, validation, push, and apply transaction used by manual, watcher, and polling triggers. Periodic polling remains enabled as repair.

## References

- [Git protocol version 2](https://git-scm.com/docs/gitprotocol-v2)
- [CloudEvents 1.0 specification](https://github.com/cloudevents/spec/blob/ce@stable/cloudevents/spec.md)
- [CDEvents source-control vocabulary](https://github.com/cdevents/spec/blob/main/source-code-version-control.md)
