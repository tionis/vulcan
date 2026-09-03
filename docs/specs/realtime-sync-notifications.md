# Realtime sync notifications

**Status:** Version 1 implementation contract

This document defines the deliberately small wake-up mechanism used by Git-backed Vulcan sync.
It is not an event protocol: notification content is never authoritative and is not interpreted.
Ordinary authenticated Git remains the only source of repository and ref state.

## Advertisement

A repository may advertise one notification endpoint through the exact Git ref
`refs/vulcan/notifications`. The ref points to a commit whose root tree contains one regular file
named `notification.json`:

```json
{
  "version": 1,
  "transport": "http_long_poll",
  "subscribe_url": "https://patch.example/h/opaque-channel?pubsub=true"
}
```

Version 1 requires those three fields. Consumers ignore unknown object fields, reject unsupported
versions or transports, and bound both the advertisement and URL lengths. `subscribe_url` must be
an absolute HTTPS URL without user information or a fragment. Plain HTTP is allowed only for an
explicitly loopback endpoint so local conformance tests do not weaken remote transport policy.

The commit should be parentless because the advertisement is mutable configuration, not history.
Publishers replace the ref with an exact compare-and-swap lease. The ref is never checked out and
is outside ordinary branch and tag namespaces. The commit carries the publisher's Git identity
(repository configuration falling back to global configuration), attributing who advertised the
endpoint; it is an ordinary commit in this respect, not a bot-authored one.

The subscribe URL is a read capability. It is confidential repository data and is available to
every principal that can fetch the advertisement ref. Vulcan must never print or log the complete
URL; diagnostics identify only its origin and a stable non-reversible fingerprint.

## Publishing

Repository administrators configure the forge webhook with a separate publish-only URL or secret.
That authority must not appear in `notification.json`. A Patchwork-style forward-hook pub/sub
channel is the reference deployment: the forge POST returns immediately and broadcasts a wake-up
to every currently connected subscriber.

Webhook payloads need no normalization. The relay may forward them, replace them with an empty
body, or omit them. Clients ignore response headers and body bytes except for enforcing transport
limits. A party that can publish can cause extra verified sync attempts but cannot supply Git state.

## Consumer behavior

For each active registered Git wiki, the daemon:

1. Queries and, when changed, fetches `refs/vulcan/notifications` through the wiki's configured Git
   remote using ordinary Git authentication.
2. Reads and validates `notification.json` from the advertised commit without checking it out.
3. Opens one interruptible HTTP GET to the advertised endpoint.
4. Treats any 2xx response as one wake-up and enqueues `SyncJobTrigger::RemoteNotification` for
   that wiki.
5. Drops the response body, reconnects, and relies on supervisor coalescing to bound bursts.
6. Retries timeouts and transient failures with bounded exponential backoff and jitter.
7. Re-reads the advertisement at daemon startup, after a wake-up, and during periodic
   reconciliation so endpoint rotation repairs itself even when the final old-channel wake-up was
   missed.

Redirects are not followed. Endpoint changes must be published through the Git advertisement so a
relay response cannot silently move credential-bearing traffic to another authority.

A missing advertisement ref disables realtime wake-up for that wiki without affecting finite sync.
Malformed or unsupported advertisements produce bounded diagnostics and likewise fall back to
polling. Pausing or unregistering a wiki stops its listener.

## Correctness and security

- Every wake-up enters the existing finite capture, fetch, merge, validation, conflict-preservation,
  publication, and apply transaction. The listener never calls Git reconciliation directly.
- Startup and periodic polling remain mandatory. Ephemeral delivery may lose events while a device
  is disconnected without affecting eventual correctness.
- The registered wiki determines the local path, remote, and live ref. Nothing received over HTTP
  can select them.
- The wiki's effective permission profile must allow both Git access and network access to the
  endpoint before the listener connects.
- TLS certificates are validated normally. URLs containing credentials in the authority component,
  fragments, control characters, or unsupported schemes are rejected.
- Listener shutdown is cooperative and bounded; daemon shutdown must not wait for a server's full
  long-poll duration.

## Deferred extensions

Protocol-level subscription aggregation (fewer long-polls than wikis), retained delivery,
structured events, client registration, mobile push, OIDC administration, and forge-native
streams are intentionally outside version 1. They require a new transport or a later additive
advertisement version only after measured deployments justify the additional control plane.
Transport-level pooling is not aggregation in this sense: the daemon shares one HTTP client
across listeners so same-origin subscriptions multiplex over HTTP/2 when negotiated. HTTP/3
remains future evaluation (see ROADMAP 12.13.3).
