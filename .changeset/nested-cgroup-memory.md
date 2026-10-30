---
"@reddb-io/cli": patch
---

Detect the memory limit of the current cgroup and its ancestors when RedDB runs in a nested Linux service, instead of incorrectly sizing memory pools from host RAM. Keep explicit configuration precedence and namespaced container detection unchanged.
