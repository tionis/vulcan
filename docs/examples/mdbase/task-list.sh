#!/bin/sh
# List an mdbase collection's tasks with a given status through the shared
# query service. Needs only the vulcan binary; no daemon or App package.
#
# Usage: task-list.sh VAULT [STATUS]   (STATUS defaults to "open")
# Set VULCAN to use a binary that is not on PATH.
set -eu
vault=${1:?usage: task-list.sh VAULT [STATUS]}
status=${2:-open}
vulcan=${VULCAN:-vulcan}
case $status in
    '' | *[!A-Za-z0-9_-]*)
        echo "task-list.sh: STATUS must be letters, digits, '-' or '_'" >&2
        exit 2
        ;;
esac
exec "$vulcan" --vault "$vault" mdbase query "types: [task]
where: 'status == \"$status\"'
order_by: [{field: title}]
select: [title]"
