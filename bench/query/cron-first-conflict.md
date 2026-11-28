# First conflict for a frequently queried room

Keep on-demand CRON expansion as the default. For repeated availability checks
on the same visible schedule, a compact index over a finite horizon is worth
prototyping in the SQL executor. The strongest case in this experiment was a
room with many short bookings and an occasional long reservation.

## The lookup

For one room, sort its occurrences by `(start, rule_id)`. Keep an occurrence
when its end exceeds every earlier end. For a nonempty request `[a, b)`, binary
search the retained ends for the first value greater than `a`. That booking is
the first conflict if its start is less than `b`. Otherwise the room is free.
An empty request has no conflicts.

The first booking whose end exceeds `a` must set a new maximum, since all
earlier ends are at most `a`. If that booking starts at or after `b`, every
later booking also starts too late. This preserves the earliest start and
the rule ID tie-break, including bookings carried in from the previous day.
Building the summary from sorted input takes one pass. Lookup takes
`O(log k)` for `k` retained entries. The full day agenda still uses every
occurrence, including bookings omitted by the summary.

## Measured cost

Each fixture represents one room for seven days. All rules in the uniform
fixture last 30 minutes. In the mixed fixture one weekly booking lasts 36 hours
and the others last 5–45 minutes. The experiment checks 168 distinct
15-minute requests per fixture. Each of the three paths must return the exact
same start, end and rule ID as a full scan, or agree that the room is free.

The table shows warm lookup times in **nanoseconds per query**. Each pair is
**occupied / free**. The ordered path uses a sorted in-memory array and the
room's true maximum duration to bound its search.

| Rules | Durations | From CRON rules | Ordered by start | Increasing ends | Summary entries / occurrences |
| ---: | --- | ---: | ---: | ---: | ---: |
| 4 | Uniform | 241 / 221 | 14 / 14 | 12 / 11 | 28 / 28 |
| 4 | Mixed | 250 / 245 | 16 / 24 | 12 / 11 | 20 / 23 |
| 64 | Uniform | 3,445 / 3,440 | 25 / 22 | 22 / 20 | 448 / 448 |
| 64 | Mixed | 3,478 / 3,492 | 104 / 130 | 20 / 17 | 190 / 443 |
| 1,024 | Uniform | 54,956 / 55,613 | 54 / 41 | 48 / 32 | 4,200 / 7,168 |
| 1,024 | Mixed | 54,634 / 54,534 | 1,128 / 1,536 | 26 / 22 | 954 / 7,163 |

On free windows the uniform fixtures examined zero candidate bookings after
the ordered seek. With a 36-hour lookback the ordered path examined an
average of 4.6, 92.2 and 1,491.8 bookings as the room grew.

For 1,024 mixed rules, expanding the week, sorting it and building the
summary took **840 µs**. Given already sorted occurrences, the summary alone
took **11.5 µs**. At these measured costs, about **16 free-window queries**
recover the full in-memory build relative to on-demand expansion, or about
**8** recover just the summary build relative to scanning the ordered
occurrences. These estimates assume the same reusable horizon and unchanged
visibility. Persistent writes, synchronization and SQL overhead need a
separate end-to-end measurement.

## Validity and reproduction

The fixture includes earlier starts whose occupancy crosses the horizon
boundary. A database implementation would need to prove coverage of the
requested window, freshness for the reader's snapshot, and the exact visible
set of bookings. Canceling a long reservation or applying a row-level filter
can expose another reservation the summary omitted. On an invalid or missing
summary the current CRON query can supply the answer. Enforcing a booking
constraint on concurrent writes also requires transactional coordination.

The measurements are from five independent single-threaded process runs on
a Ryzen 5 3600 under Windows, with low process priority and affinity to two
logical CPUs. Each run uses the median of seven warm batch averages per
lookup path and the median of seven builds from parsed UTC rules. The table
reports the median across runs. These are in-memory algorithm timings.
The earlier SQL benchmark covers end-to-end daily agenda and conflict reads.
[SQL benchmark](../../crates/reddb-server/tests/cron_agenda_bench.rs)

```sh
cargo test -j 1 --profile release-fast -p reddb-io-types --test cron_conflict_index -- --include-ignored --nocapture --test-threads=1
```
