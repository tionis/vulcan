---
status: draft
reviewed: true
---

priority:: 2
(priority:: 3)
month:: 2026-04
duration:: 1d 3h
choices:: "alpha", "beta"
plain:: alpha, beta
**Due Date**:: 2026-04
Noël:: un jeu de console
[🎅:: gifts]
reviewed:: false
[owner:: [[People/Bob]]]

## Lists
- Plain list item [kind:: note]
  1. Nested numbered item ^list-child

## Tasks
- [ ] Write docs [due:: 2026-04-01]
  - [x] Ship release [owner:: [[People/Bob]]]

`= this.status`

```dataview
TABLE status, priority
FROM #project
WHERE reviewed = true
SORT file.name ASC
```

```dataviewjs
dv.table(["Status"], [[this.status]])
```

#project
