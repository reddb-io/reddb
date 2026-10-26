# Admit recovered native sources before initial audit mutations

The six-event real-ledger reproduction previously failed its one-row cold
admission requirement: the warm append applied one delta row, while the first
append after reopen used full reconstruction. Both byte equivalence and recovery
already passed. See the adjacent frozen `cold-ledger-first-publication` probe.

The runtime change admits persisted native source proofs while finishing a
writable paged open, after authoritative recovery and metadata initialization
and before runtime bootstrap or audited requests mutate the collections. It
reuses the existing source digest, exact page/summary and payload validation;
an unavailable or mismatching proof retains conservative full reconstruction.
No format, WAL barrier, protected page publication or retirement order changes.
Read-only opens keep their existing behavior.

The original reproduction is now a normal regression test. It passes with one
warm and one cold delta row, exact full-derived frame bytes at both stages, and
recovery of all eight events including the final acknowledged ID. All 11 native
source tests pass, including overlapping checkpoint admission, corruption,
legacy fallback and six subprocess recovery phases. The cold-overlap control
now checks that the unchanged source is admitted before the runtime can mutate
it, while retaining its existing competing publication/reference assertions.

## Timing limit and outstanding qualification

The first corrected miniature observation reports 83,205 microseconds opening
and 58,698 microseconds first publication (141,903 combined). The preceding
failure observed 136,147 combined. These are single small runs with real UUIDv7
audit IDs; they do not establish a latency improvement. The change repairs
incremental admission, not constant-time text publication. Decoding/serialization
still traverses the full bounded text frame; source validation is still linear.

Historical qualification must measure opening plus first publication and the
individual stages under identical data, limits and storage. Do not count moving
cold validation into startup as reduced work. RC378 remains rejected. The strict
10-second/64 MiB gates and the same-final-artifact Auth/Platform, held-snapshot,
crash recovery and shutdown requirements remain outstanding. Recovery integration
tests are being run on this patch before a new candidate is admitted.

Private local results: `stg-outage-20261026-2002/cold-ledger-fix-prep1399.private.log`
under the existing qualification root. Local Rust uses the accepted one-at-a-time
8 GiB RAM, 4 GiB swap and two-CPU scope.
