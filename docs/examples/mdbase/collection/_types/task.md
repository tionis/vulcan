---
kind: mdbase.type
name: task
version: 1
description: A unit of work.
match:
  where:
    type: task
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [type, title]
    properties:
      type: { const: task }
      id: { type: string }
      title: { type: string }
      status: { enum: [open, active, done] }
      priority: { type: integer, minimum: 1, maximum: 5 }
      project: { type: string }
collection:
  read_defaults:
    status: open
  unique:
    - field: id
      scope: type
  links:
    project: { target_type: project, validate_exists: true }
lifecycle:
  on_create:
    set:
      id: { ulid: true }
---

# Task

Tasks belong to a project and default to `open`. Vulcan generates `id` on creation.
