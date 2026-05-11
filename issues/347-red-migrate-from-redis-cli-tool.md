# red migrate-from-redis CLI tool

GitHub: local follow-up from reddb-io/reddb#340

Labels: needs-triage

GitHub issue number: #347

## Parent

#340 (https://github.com/reddb-io/reddb/issues/340)

## What to build

Implement `red migrate-from-redis` as an explicit CLI for the Redis to
Blob Cache migration playbook, or reject the CLI surface in favor of
documented application-owned helpers.

## Acceptance criteria

- [ ] The CLI status is explicit in docs and command help.
- [ ] The command supports a dry-run mode that validates Redis and
      RedDB connectivity without writing cache entries.
- [ ] The command can run the dual-write shadow phase or emits a clear
      unsupported error that points to the application-owned helper
      pattern.
- [ ] Any supported execution path records mismatch counts and exit
      status suitable for automation.
- [ ] Public tests cover command help, dry-run behavior, and the
      implemented or explicitly rejected dual-write mode.

## Blocked by

None.
