# Realtime sync notifications (retired)

**Status:** Retired. Version 1 shipped a wake-up mechanism for Git-backed sync: a
`refs/vulcan/notifications` ref advertising an HTTPS long-poll endpoint, fed by a forge webhook
through a relay. It was removed because every repository needed a custom ref, a webhook, and a
relay set up by hand, and the custom ref interacted badly with forge permissions.

The daemon now discovers remote changes by adaptive per-wiki polling
(`sync.poll_min_seconds`, `sync.poll_max_seconds`); see
[git-sync-architecture.md](git-sync-architecture.md). Vulcan no longer reads the advertisement
ref, and `vulcan sync advertise`, `unadvertise`, and `notifications` are gone. A repository that
still carries the ref can drop it with `git push origin :refs/vulcan/notifications`.
