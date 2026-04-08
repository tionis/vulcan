---
status: draft
reviewed: true
---

owner:: [[People/Bob]]

# Release Dashboard

```dataview
TABLE status, due
FROM "TaskNotes/Tasks"
WHERE status != "done"
SORT due ASC
```

```dataview
TABLE file.day, project
FROM "Journal/Daily"
SORT file.day ASC
```
