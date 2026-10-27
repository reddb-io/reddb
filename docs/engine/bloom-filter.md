# Segment membership pruning

The internal bloom-compatible probe checks physical entity membership without
materializing matching rows. The SQL query path extracts only equality probes
on `rid`; ordinary fields, including a field named `id`, are not implicit primary
keys. Declared user indexes have their own lookup paths.

A supported physical RID is checked against the growing and sealed segment
storage. A negative result can skip the query only when every consulted segment
proves absence. Missing state and unsupported probes remain conservative. MVCC
visibility and the complete query predicate still apply to positive candidates.

The old first-column map of debug-formatted values is removed. It did not serve
this RID path and could add a second resident index during restoration and
consolidation. Physical membership uses the existing entity directories, so no
additional per-row bloom map is allocated by this probe.

Segment publication, deletion and consolidation maintain the same physical
entity lookup. There is no configurable false-positive rate for this exact
membership path. The method name `bloom_may_contain_key` remains compatible.

See [B-tree index](/engine/btree.md) and
[resident row accounting](/architecture/resident-row-memory-accounting.md).
