---
kind: mdbase.type
name: project
version: 1
match:
  where:
    type: project
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [type, id, title]
    properties:
      type: { const: project }
      id: { type: string }
      title: { type: string }
---
