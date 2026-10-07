---
type: view
id: task.views
version: 1
name: Task views
properties:
  title:
    label: Task
query:
  types: [task]
  where: 'status != "done"'
views:
  - id: by-priority
    name: Open work by priority
    select: [title, status, priority]
    order_by:
      - field: priority
        direction: desc
    presentation:
      type: tasknotes.task-list
  - id: for-project
    name: Tasks for a project
    context:
      this:
        on_missing: error
        types: [project]
    where: 'project == "[[" + this.id + "]]"'
    select: [title, status]
    group_by:
      - field: status
    summaries:
      - field: title
        function: count
        name: tasks
---

Saved task views for the example collection.
