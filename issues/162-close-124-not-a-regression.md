# Bench: close #124 with 'not a regression — methodology drift' evidence [AFK]

GitHub: reddb-io/reddb#162
Parent: #152
Blocked by: #154

Issue #124 reports f63a4f3 regresses insert_bulk and delete_sequential vs c95ceb7. Under same focused-loop config, f63a4f3 is faster than rebuilt c95ceb7 on both. Original report compared cross-configurations. Doc-only slice.

## Acceptance Criteria

- [ ] Comment on #124 with four-session evidence table (sess-20260506075712-547721, ...113532-927600, ...122435-1038164, ...125739-1116474).
- [ ] Comment cross-links #154 as systemic fix.
- [ ] #124 closed as `not planned` / not a regression.
- [ ] PRD #152 cross-linked from close comment.
