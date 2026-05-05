# Host kernel knobs for `perf` profiling

Capturing a flamegraph of the `red` server with `perf record` requires
two host-level kernel sysctls to be relaxed. Modern Ubuntu / Pop!_OS
ship with both at their most restrictive defaults, which is why the
P5 punch-list item in
[`insert_sequential-2026-05-05.md`](insert_sequential-2026-05-05.md)
could not produce a live profile.

This doc is the operator-side companion for the `make perf-bench`
target.

## The two knobs

### `kernel.perf_event_paranoid`

Controls who can call `perf_event_open(2)`.

| value | meaning                                                                      |
|------:|------------------------------------------------------------------------------|
|    -1 | unrestricted; any user can profile any process and read raw tracepoints.     |
|     0 | allow CPU events + kernel profiling for unprivileged users.                  |
|     1 | allow CPU events + user-space profiling for unprivileged users (**minimum needed for `perf record -g` on your own process**). |
|     2 | only allow user-space measurements (no CPU/kernel events).                   |
|     3 | disable all unprivileged `perf_event_open`.                                  |
|     4 | Ubuntu's hardened default — same as 3, plus extra cgroup restrictions.       |

`make perf-bench` requires `<= 1`.

### `kernel.yama.ptrace_scope`

Controls who can `ptrace(2)` / attach to a running process.

| value | meaning                                                              |
|------:|----------------------------------------------------------------------|
|     0 | classic Unix — any process owned by the same uid can be attached.    |
|     1 | only direct parent → child attachment (Ubuntu default).              |
|     2 | only with `CAP_SYS_PTRACE`.                                          |
|     3 | ptrace disabled.                                                     |

`perf record -p <pid>` against a server you launched in a different
shell needs `0` (or you must run `perf` as root).

## Relaxing them for a profiling session

Effective until the next reboot:

```bash
sudo sysctl kernel.perf_event_paranoid=1 kernel.yama.ptrace_scope=0
```

Verify:

```bash
sysctl kernel.perf_event_paranoid kernel.yama.ptrace_scope
# expect:
#   kernel.perf_event_paranoid = 1
#   kernel.yama.ptrace_scope = 0
```

## Making it permanent (dev hosts only)

Drop a file in `/etc/sysctl.d/`:

```bash
sudo tee /etc/sysctl.d/99-reddb-perf.conf <<'EOF'
kernel.perf_event_paranoid = 1
kernel.yama.ptrace_scope = 0
EOF
sudo sysctl --system
```

### Security trade-off

These knobs are restrictive **for good reasons**:

- `perf_event_paranoid <= 1` lets any local user attach to and read
  the instruction stream of any process they own. On a shared host
  that includes their browser, ssh-agent, GPG processes, etc. — every
  in-memory secret becomes readable by a sibling shell.
- `ptrace_scope = 0` enables the same window via the older
  `PTRACE_ATTACH` path, which is what most credential-stealing PoCs
  actually use.

Only relax these on workstations / dev VMs where you are the only
user. Never on a shared bench host, CI runner, or production box. If
you need profiling on a shared host, run `perf` as root from a sudo
shell and skip the sysctl change entirely.

## Running the bench profile

Once the knobs are relaxed:

```bash
make perf-bench
```

The target writes the flamegraph to:

```
target/perf/insert_sequential.svg
```

It also leaves the raw `perf.data` next to the SVG so you can re-run
`perf report` / `perf script` against it without re-recording.

The bench load that drives the server during the 30-second `perf
record` window comes from
`/home/cyber/Work/reddb.io/rdb-benchmark` (separate repo). If the
single bench-runner pass finishes before the 30-second window (small
`--items`), open another terminal and re-run the bench loop until
`perf record` exits — the recorder samples whatever the server is
doing for the full 30 s either way.

## Restoring the defaults

After profiling:

```bash
sudo sysctl kernel.perf_event_paranoid=4 kernel.yama.ptrace_scope=1
```

(or just reboot if you never persisted the override).
