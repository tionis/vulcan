# Clippings Dashboard

This note contains a triage view.

```dataview
TABLE
    file.link AS Clipping,
    choice(triage_status, triage_status, "new") AS Triage
  FROM "00-09 Management & Meta/00 Inbox/00.12 Clippings"
  WHERE file.name != this.file.name
    AND (
      !triage_status
      OR triage_status = "new"
      OR triage_status = "split"
    )
  SORT file.ctime ASC
  LIMIT 100
```
