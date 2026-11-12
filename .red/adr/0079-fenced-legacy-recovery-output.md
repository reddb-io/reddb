# Fence legacy recovery output against writes

Status: accepted
Date: 2026-11-06

Legacy paged catalogs do not preserve a trustworthy identity high-water mark
when a collection page is lost. Reopening a recovered copy for writes could
reuse an identity that existed in the damaged collection or in an external
system. A loss report alone cannot prevent that error.

Reserve paged file version `0x0001_0006` for recovery output whose identity
high-water mark is unknown. Both superblock copies must be durably fenced to
this version before publishing the recovered filename. A pager must reject a
writable open before double-write or WAL recovery, and may open the file only
with its read-only configuration. Older engines reject the newer version rather
than silently writing the result. Ordinary new databases continue using their
existing format versions.

The fence is not an identity-reconciliation protocol. There is no in-place
unfence operation. The operator must reconcile identities against an
authoritative source and export verified records into a new store. A page
checksum does not prove the page is the latest published generation.

## Amendment — capability-aware catalog roots (2026-11-12)

Copy-on-write catalogs require the capability-aware file header introduced in
`0x0001_0009`. Recovery output using that catalog format must retain this newer
version and declare `FILE_CAPABILITY_RECOVERY_READ_ONLY` in both superblocks;
lowering its version to `0x0001_0006` would hide the catalog-root capability and
lose its independent root reference. This extends the original version-only
encoding while preserving its contract. Legacy `0x0001_0006` files remain
read-only, and a writer rejects either encoding before DWB/WAL recovery. Offline
rewrites preserve the fence; no in-place unfence operation is introduced.
