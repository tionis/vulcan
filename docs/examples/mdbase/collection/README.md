# Example mdbase collection

A small, valid collection for trying the `vulcan mdbase` commands and the scripts beside it:

```sh
vulcan --vault docs/examples/mdbase/collection mdbase validate
vulcan --vault docs/examples/mdbase/collection mdbase schema task
vulcan --vault docs/examples/mdbase/collection mdbase views
vulcan --vault docs/examples/mdbase/collection mdbase view task.views by-priority
vulcan --vault docs/examples/mdbase/collection mdbase view task.views for-project --context projects/docs.md
vulcan --vault docs/examples/mdbase/collection init   # once, for Bases evaluation,
vulcan --vault docs/examples/mdbase/collection scan   # which reads the note index
vulcan --vault docs/examples/mdbase/collection mdbase view views/tasks.base all-tasks
docs/examples/mdbase/task-list.sh docs/examples/mdbase/collection open
python3 docs/examples/mdbase/collection.py docs/examples/mdbase/collection
```

It has three types (`task`, `project`, `view`), task records linked to a project with a unique
`id`, read defaults and a lifecycle-generated `id`, a saved-view record, and an Obsidian `.base`
selected by `x-obsidian.bases.include`. Commands may create a rebuildable `.vulcan/` cache; delete it
freely.
