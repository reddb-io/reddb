---
"@reddb-io/cli": patch
---

Overdue-execution warnings now also name HTTP executions: the route
(`HTTP POST /query`) and the statement, repeated every minute while the
execution stays stuck. Before, only wire executions carried a statement and
HTTP ones were logged as `<unlabelled execution>` (#2908).
