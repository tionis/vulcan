# Investigation: `web fetch` extraction backends

**Date:** 7 April 2026  
**Status:** Keep the built-in local extraction path as the default; defer Tavily Extract and Firecrawl integration

## Question

Should `vulcan web fetch` switch from the current built-in local extraction path to Tavily Extract or Firecrawl?

## Summary

No, not as the default path in Phase 9.19.10.

Both Tavily Extract and Firecrawl solve a broader problem than Vulcan's current `web fetch`:

- They are remote extraction products, not just local HTML-to-markdown cleanup.
- They require authenticated API access.
- They are strongest when the user wants structured extraction, schema-guided output, or LLM-assisted page processing.

That is useful future capability, but it is a worse default for Vulcan's current `web fetch` contract, which is intentionally simple, deterministic, and usable without any external account.

## Current Vulcan behavior

- `web fetch` performs a normal HTTP GET with a Vulcan user agent.
- It does a best-effort `robots.txt` check.
- In markdown mode it runs a local `rs-trafilatura` main-content extraction.
- If no readable main content is found, callers can fall back to `html` or `raw`.
- It works with zero provider setup and no per-request network dependency beyond the target page itself.

That makes it a good default for CLI usage, scripts, and the JS sandbox.

## Tavily Extract

What it adds:

- API-driven extraction rather than local parsing.
- Stronger support for LLM-oriented research flows.
- Good fit when the caller wants normalized remote extraction as a service.

Why not make it the default:

- Requires an API key and remote provider dependency.
- Changes `web fetch` from a direct fetch utility into a provider-mediated workflow.
- Adds cost, latency, and another failure mode for a command that currently works offline except for the target site.

## Firecrawl

What it adds:

- A richer extraction API with schema/prompt-driven structured extraction.
- Broader crawling-oriented controls than Vulcan currently needs for one-shot fetch.

Why not make it the default:

- Also requires authenticated remote API access.
- Best fit is structured extraction and crawl workflows, not a lightweight markdown fetch.
- Introduces more product surface than Phase 9.19.10 needs.

## Decision

For now:

- Keep the built-in local extraction path as the default `web fetch` path.
- Treat Tavily Extract and Firecrawl as future optional backends, not replacements.
- Revisit provider-backed extraction only when Vulcan adds an explicit structured extraction mode such as:
  - `web fetch --backend tavily --schema ...`
  - `web fetch --backend firecrawl --prompt ...`
  - or a separate `web extract` command

## Recommendation for Phase 9.19.10

Ship:

- DuckDuckGo as the no-key default for `web search`
- clearer `[web.search]` config docs and examples
- no change to the default `web fetch` extraction path

Defer:

- Tavily Extract integration
- Firecrawl integration
- any schema-guided or prompt-guided extraction surface

## Sources

- Tavily docs: https://docs.tavily.com/
- Firecrawl Extract docs: https://docs.firecrawl.dev/api-reference/v1-endpoint/extract
