# Vendored mathjs

`math.js` is the browser bundle of [mathjs](https://mathjs.org) 15.2.0, unmodified, from
`https://registry.npmjs.org/mathjs/-/mathjs-15.2.0.tgz` (`lib/browser/math.js`), sha1 of the
tarball `ddfbf50a9aeaec2edfb524195e8202ca29f8e757`.

Vulcan evaluates it in an isolated QuickJS context for Numerals calculator blocks and Meta Bind
`math` views (`docs/specs/live-documents.md`, Roadmap 9.42.4). Numerals 1.10.2 pins this version.

mathjs is Apache-2.0 (`LICENSE`, `NOTICE`); the bundle includes MIT-licensed dependencies listed
in `math.js.LICENSE.txt`.

To update, replace `math.js` and the license files from the new release's tarball, update the
version and checksum here, and rerun the Numerals and Meta Bind tests.
