# Forgejo custom-ref conformance gate

## Decision boundary

Vulcan version 1 keeps the canonical remote live tip at
`refs/heads/__vulcan-sync/live` and retired epochs below
`refs/heads/__vulcan-sync/epochs/`. Local operational state remains below
`refs/vulcan/` and is never published.

The installed Git CLI has an automated integration test proving that a bare Git
remote accepts fetch, compare-and-swap push, and exact-lease deletion for the
non-branch ref `refs/vulcan-sync/v1/live`. That establishes Git transport
capability, not Forgejo hosting behavior. A custom remote namespace must not
become the default until the actual deployment passes every check below.

## Deployment checklist

Record the Forgejo version, repository configuration, authentication mode, and
test date with the results.

- [ ] Push a new custom live ref and verify it is advertised to a fresh client.
- [ ] Fetch the exact custom ref into a device-local ref without broad wildcard
      fetching.
- [ ] Advance it with an exact expected-old-object lease and verify a stale
      writer is rejected.
- [ ] Delete an epoch/custom ref with an exact lease and verify a stale deletion
      is rejected.
- [ ] Confirm repository and branch-protection permissions cannot accidentally
      bypass Vulcan's intended writers or reject every valid writer.
- [ ] Confirm push webhooks include custom-ref create, update, and delete events
      with enough identity to schedule the ordinary finite sync transaction.
- [ ] Confirm garbage collection and repository maintenance retain every object
      reachable only through the custom namespace.
- [ ] Confirm backup, mirror, migration, and restore operations preserve the
      namespace exactly.
- [ ] Confirm Forgejo Actions triggers and ref filters have documented behavior
      for the namespace, even if Vulcan does not initially depend on Actions.
- [ ] Repeat the checks after the next deployed Forgejo upgrade before changing
      the production default.

## Promotion rule

Passing the checklist permits a design decision; it does not silently migrate
existing repositories. Any custom-ref default requires a new ref-namespace
version, an explicit compatibility/migration plan, mixed-client tests, and a
hidden-branch fallback for hosts that cannot meet the same safety properties.
