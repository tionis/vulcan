---
status: draft
reviewed: true
---

priority:: 2
(priority:: 3)
[owner:: [[People/Bob]]]

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
