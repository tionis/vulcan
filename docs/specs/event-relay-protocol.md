# Event Relay Protocol

**Status:** Draft 0.1  
**Audience:** Relay implementers, event producers, and event consumers  
**Normative base:** CloudEvents 1.0

This document defines a small, application-neutral protocol for discovering and subscribing to realtime event channels. It deliberately does not define Git, Vulcan, wiki, or synchronization semantics. Domain profiles define the CloudEvent types carried by a channel; the first profile is [Git Realtime Events](git-realtime-events.md).

The key words **MUST**, **MUST NOT**, **REQUIRED**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described by RFC 2119 and RFC 8174 when they appear in uppercase.

## 1. Goals and non-goals

The protocol provides:

- transport-neutral CloudEvent delivery;
- discovery metadata for one event source or channel;
- public and capability-authenticated read subscriptions;
- separately scoped publisher and subscriber authority;
- declared retention and replay behavior;
- extension through versioned domain profiles and transport bindings.

Version 1 does not define a general RPC system, command execution, user directory, cross-relay federation, or exactly-once application processing. A relay transports notifications; it is not the authority for the state described by an event.

## 2. Terms

- **Event source:** the system in which an event originates.
- **Adapter:** a component that validates a source-specific message and produces a normalized CloudEvent.
- **Relay:** a service that accepts CloudEvents from authorized publishers and delivers them to authorized subscribers.
- **Channel:** an opaque authorization and delivery boundary containing events.
- **Profile:** a versioned contract defining domain-specific event types, schemas, and consumer rules.
- **Source descriptor:** non-secret discovery metadata describing available bindings and authorization methods.
- **Subscription bundle:** a confidential, portable document containing the authority needed to subscribe.
- **Binding:** the mapping of this protocol to NATS, MQTT, HTTP, WebSocket, or another transport.

## 3. Layering

```text
domain event profile (for example Git Realtime Events)
                         |
CloudEvents 1.0 event format and context
                         |
Event Relay discovery, authorization, and delivery contract
                         |
NATS / MQTT / WebSocket / HTTP binding
```

Relay implementations MUST NOT require knowledge of a domain profile merely to carry its events. They MAY validate a profile when its schema is installed and MUST report whether validation is enforced.

## 4. Channel and event model

A channel identifier MUST be opaque, stable for the lifetime of the channel, URL-safe, and contain at least 128 bits of unguessable entropy when channel discovery itself is private. It is an identifier, not an authentication secret.

Every delivered event MUST be a valid CloudEvent 1.0. Structured JSON is the REQUIRED event format for version 1 bindings. A relay MUST preserve the CloudEvent context and data supplied by the normalized publisher. Transport metadata such as broker sequence or delivery attempt SHOULD remain transport metadata rather than changing the domain event.

The tuple `(source, id)` identifies a distinct CloudEvent. Relays MAY deduplicate that tuple, but consumers MUST tolerate duplicate delivery. Consumers MUST also tolerate reconnects, gaps, and reordering unless a binding and source descriptor explicitly provide a stronger guarantee.

Relays MUST advertise a maximum event size. The version 1 default is 256 KiB when no smaller limit is declared. Profiles SHOULD define tighter limits and MUST NOT use the relay to transfer authoritative large objects when a stable object protocol exists.

## 5. Retention rules

A channel declares one or more ordered retention rules. Each rule has a stable `id`, matches an explicit set of CloudEvent `type` values or `*`, and selects one of the following classes:

- `ephemeral`: only currently connected subscribers are expected to receive an event.
- `bounded_log`: events are retained within declared time and count limits and can be replayed from a binding-specific cursor.
- `latest_by_subject`: at most the newest event for each `(type, source, subject)` key is retained. Events without a `subject` are invalid for this class.

The first matching rule applies. A descriptor MUST NOT contain overlapping rules whose order could change the advertised durability of the same event type. Event types not matched by a rule are rejected unless the descriptor explicitly ends with an `ephemeral` wildcard rule.

Retention is a delivery aid, not an authority guarantee. A consumer recovering from a gap MUST reconcile with the event source when the domain profile identifies an authoritative source.

## 6. Source descriptor

A source descriptor is safe to publish and MUST NOT contain bearer secrets, private keys, webhook signing secrets, or credentials embedded in URLs. Its media type is `application/vnd.event-relay.source+json`.

```json
{
  "spec": "event-relay/1",
  "id": "urn:event-relay-channel:01K00000000000000000000000",
  "profiles": [
    "https://tionis.dev/spec/git-realtime/1"
  ],
  "bindings": [
    {
      "type": "nats",
      "endpoint": "tls://events.example.net:4222",
      "subject_filter": "events.channels.01K00000000000000000000000.>"
    }
  ],
  "authorization": ["public", "bearer_capability"],
  "retention": [
    {
      "id": "git-updates",
      "types": ["dev.tionis.git.refs.updated.v1"],
      "class": "bounded_log",
      "max_age_seconds": 86400
    },
    {
      "id": "git-state",
      "types": ["dev.tionis.git.ref.state.v1"],
      "class": "latest_by_subject"
    }
  ],
  "limits": {
    "event_bytes": 65536
  }
}
```

Required fields are `spec`, `id`, `profiles`, `bindings`, `authorization`, `retention`, and `limits.event_bytes`. Retention rule IDs MUST use lowercase ASCII letters, digits, and hyphens and be unique within the descriptor. Consumers MUST reject unsupported major versions and MUST ignore unknown fields. A binding MAY add namespaced fields but MUST NOT reinterpret the common fields.

An HTTPS descriptor endpoint SHOULD use ordinary HTTP cache validators. A consumer MUST NOT infer a descriptor URL from a source URL unless a domain profile defines that discovery rule.

## 7. Subscription bundle

A subscription bundle combines a source descriptor with confidential subscriber authority. Its media type is `application/vnd.event-relay.subscription+json`.

```json
{
  "spec": "event-relay-subscription/1",
  "descriptor": {
    "spec": "event-relay/1",
    "id": "urn:event-relay-channel:01K00000000000000000000000",
    "profiles": ["https://tionis.dev/spec/git-realtime/1"],
    "bindings": [
      {
        "type": "nats",
        "endpoint": "tls://events.example.net:4222",
        "subject_filter": "events.channels.01K00000000000000000000000.>"
      }
    ],
    "authorization": ["bearer_capability"],
    "retention": [
      {
        "id": "git-updates",
        "types": ["dev.tionis.git.refs.updated.v1"],
        "class": "bounded_log",
        "max_age_seconds": 86400
      },
      {
        "id": "git-state",
        "types": ["dev.tionis.git.ref.state.v1"],
        "class": "latest_by_subject"
      }
    ],
    "limits": { "event_bytes": 65536 }
  },
  "credential": {
    "scheme": "bearer_capability",
    "token": "REDACTED"
  }
}
```

A subscription bundle MUST be handled as a credential. Implementations MUST redact its token from normal output, debug formatting, telemetry, process arguments, URLs, and error messages. Importers SHOULD reject files readable by other users where the platform exposes reliable permission metadata. A client SHOULD place the credential in a platform secret store and persist only a non-secret credential identifier with the descriptor.

## 8. Authorization profiles

### 8.1 Public subscription

The `public` profile grants subscription without authentication. It is appropriate only when the event metadata is intentionally public.

### 8.2 Bearer capability subscription

The `bearer_capability` profile is REQUIRED for the initial private implementation. A capability token:

- MUST contain at least 256 bits generated by a cryptographically secure random source;
- MUST grant subscription only, never publication or channel administration;
- MUST be scoped to exactly one channel and its declared transport subjects;
- MUST be independently revocable;
- SHOULD be unique per subscriber;
- MUST be stored by the relay only as a verifier or cryptographic hash when opaque tokens are used;
- MUST be sent through the binding's authentication field and never in an endpoint URL.

Read-only authority is still sensitive: events can reveal repository existence, update timing, ref names, and object identifiers. A shared read token MAY be supported as an explicit low-administration mode, but implementations SHOULD default to per-subscriber credentials.

### 8.3 Publisher authority

Publisher authority MUST be distinct from subscriber authority. The initial profiles are:

- source-specific webhook verification, such as an HMAC secret;
- a random publish-only ingress capability;
- a broker credential scoped to publish only to one channel.

Compromise of a subscriber credential MUST NOT permit event injection. Compromise of a webhook secret MUST NOT permit subscription or channel administration.

### 8.4 Extensible identity authorization

A later identity profile MAY exchange OIDC, OAuth, mTLS, or signed public-key identity for short-lived binding credentials. It MUST preserve the same channel-level publish/subscribe separation. The common descriptor advertises authorization scheme names without embedding an identity provider into the core protocol.

## 9. NATS binding

The NATS binding is REQUIRED for the reference implementation. It uses the CloudEvents NATS structured-content binding with JSON payloads.

- A channel maps to one opaque NATS subject filter ending in `.>`. The **channel base** is that filter without the terminal `.>`.
- Subscriber credentials may subscribe only to that channel prefix and required broker inbox/control subjects.
- Publisher credentials may publish only below that channel prefix and may not subscribe.
- Subjects MUST NOT contain source URLs, repository names, user names, ref names, or secrets.
- An `ephemeral` rule publishes to `<channel-base>.events.<rule-id>` through Core NATS.
- A `bounded_log` rule publishes to `<channel-base>.events.<rule-id>` backed by JetStream.
- A `latest_by_subject` rule publishes to `<channel-base>.state.<rule-id>.<state-key>` backed by JetStream with at most one retained message per complete NATS subject. `state-key` is lowercase hexadecimal SHA-256 over the UTF-8 bytes `type`, one zero byte, `source`, one zero byte, and `subject`, in that order. This keeps domain identifiers out of broker subjects while making retention deterministic across conforming publishers.
- Redelivery and reconnect behavior is at-least-once from the consumer's perspective; consumers acknowledge only after validating and routing an event.

One client connection SHOULD multiplex compatible subscriptions sharing the same endpoint, TLS policy, and credential identity. Authentication or tenant boundaries MUST NOT be weakened merely to reduce connection count.

Future MQTT, WebSocket, and HTTP bindings MUST preserve the same descriptor, credential separation, CloudEvent identity, and retention meanings.

## 10. Security and privacy requirements

- Network bindings MUST use authenticated encryption outside explicitly local test deployments.
- Endpoint certificates MUST be validated; subscription bundles MAY additionally pin a trust anchor or public key.
- Adapters MUST authenticate the original source before normalization.
- Relays MUST bound event size, connection rate, publish rate, retained storage, and replay requests.
- Consumers MUST validate CloudEvents and the selected domain profile before acting.
- Relay acceptance proves only that an authorized publisher submitted an event. Domain consumers MUST still reconcile with their authoritative source.
- Channel deletion, subscriber revocation, and publisher rotation MUST be possible independently.
- Administrative audit records MUST identify credential IDs, not secret values.

End-to-end event signatures and payload encryption are deferred profiles. TLS plus a trusted relay is the initial trust model; implementations MUST NOT claim an untrusted-relay or end-to-end authenticity property until those profiles exist.

## 11. Extensibility and compatibility

Profiles are identified by stable, versioned URI strings. A profile MUST document its event types, schemas, size limits, authoritative source, retained-state keys, ordering assumptions, and consumer recovery behavior.

Additive fields are compatible within a major version. A consumer MUST ignore unknown JSON object fields but MUST reject an unsupported major protocol or profile version when the profile is required for safe handling. New authorization and transport bindings can be added without changing CloudEvent payloads.

CloudEvents describes facts. A future request/reply or command protocol MAY reuse the same broker deployment, but it MUST use a separate profile, authorization scope, and subject namespace.

## 12. Conformance

A conforming relay test suite must cover:

- valid and invalid CloudEvent admission;
- channel isolation and publish/subscribe separation;
- public and bearer-capability subscription;
- token redaction, rotation, and revocation;
- duplicate delivery and reconnect;
- each advertised retention class;
- unknown additive fields and unsupported major versions;
- event and rate limits;
- TLS and endpoint identity failures;
- adapter rejection before source authentication succeeds.

## References

- [CloudEvents 1.0 specification](https://github.com/cloudevents/spec/blob/ce@stable/cloudevents/spec.md)
- [CloudEvents JSON format](https://github.com/cloudevents/spec/blob/ce@stable/cloudevents/formats/json-format.md)
- [CloudEvents NATS binding](https://github.com/cloudevents/spec/blob/ce@stable/cloudevents/bindings/nats-protocol-binding.md)
- [CloudEvents MQTT binding](https://github.com/cloudevents/spec/blob/ce@stable/cloudevents/bindings/mqtt-protocol-binding.md)
- [CloudEvents Subscriptions draft](https://github.com/cloudevents/spec/blob/main/subscriptions/spec.md)
- [NATS security model](https://docs.nats.io/learn/security/)
