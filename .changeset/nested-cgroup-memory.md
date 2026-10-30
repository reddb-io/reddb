---
"@reddb-io/cli": patch
---

Detect the memory limit of the current cgroup and its ancestors when RedDB runs in a nested Linux service, instead of incorrectly sizing memory pools from host RAM. Keep explicit configuration precedence and namespaced container detection unchanged.

Preserve unconsumed free pages across crashes when normal writes reuse a persisted freelist outside a checkpoint ownership epoch. Keep the untouched IDs on disk instead of detaching a whole trunk into volatile memory; checkpoint/replay epochs retain journaled batch allocation.
