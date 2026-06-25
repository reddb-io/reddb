/*
 * unreliable-libc: an LD_PRELOAD fault-injection shim (DST Fatia 0, #1351).
 *
 * A direct port of Turso's `unreliable-libc` approach: interpose the real libc
 * durability calls (`write`, `pwrite`, `fsync`, `fdatasync`, `rename`) and make
 * them lie the way a real disk lies under stress -- short writes, `EIO`, and a
 * seed-driven "freeze after N syscalls then SIGKILL" power-cut.
 *
 * Everything is controlled by a single seed so a discovered failure reproduces
 * byte-for-byte. With `UNRELIABLE_SEED` unset the shim is a fully transparent
 * pass-through, so preloading it into an unrelated process changes nothing.
 *
 * Only regular-file descriptors (fd >= 3, `S_ISREG`) are faulted; stdout,
 * stderr, and pipes are left alone so the harness can still read the workload's
 * `SEED=<n>` line and so the kill counter stays deterministic regardless of
 * incidental logging.
 *
 * Build: compiled to `libunreliable_libc.so` by this crate's `build.rs` and
 * loaded via `LD_PRELOAD`. No engine source change is required.
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

typedef ssize_t (*write_fn)(int, const void *, size_t);
typedef ssize_t (*pwrite_fn)(int, const void *, size_t, off_t);
typedef int (*fsync_fn)(int);
typedef int (*rename_fn)(const char *, const char *);

static write_fn real_write = NULL;
static pwrite_fn real_pwrite = NULL;
static fsync_fn real_fsync = NULL;
static fsync_fn real_fdatasync = NULL;
static rename_fn real_rename = NULL;

/* Configuration, parsed once from the environment at load time. */
static int g_active = 0;          /* UNRELIABLE_SEED present */
static uint64_t g_seed = 0;       /* UNRELIABLE_SEED */
static int g_powercut = 0;        /* UNRELIABLE_POWERCUT=1 */
static uint64_t g_kill_after = 0; /* eligible-syscall index that SIGKILLs */
static uint64_t g_eio_ppm = 0;    /* UNRELIABLE_EIO_PPM (per-million) */
static uint64_t g_short_ppm = 0;  /* UNRELIABLE_SHORT_PPM (per-million) */

/* Monotonic count of eligible (regular-file durability) syscalls. */
static uint64_t g_counter = 0;

static uint64_t splitmix64(uint64_t x) {
  x += 0x9E3779B97F4A7C15ULL;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
  return x ^ (x >> 31);
}

static uint64_t env_u64(const char *name, uint64_t fallback) {
  const char *raw = getenv(name);
  if (raw == NULL || raw[0] == '\0') {
    return fallback;
  }
  return strtoull(raw, NULL, 10);
}

static void resolve_reals(void) {
  if (real_write == NULL) {
    real_write = (write_fn)dlsym(RTLD_NEXT, "write");
    real_pwrite = (pwrite_fn)dlsym(RTLD_NEXT, "pwrite");
    real_fsync = (fsync_fn)dlsym(RTLD_NEXT, "fsync");
    real_fdatasync = (fsync_fn)dlsym(RTLD_NEXT, "fdatasync");
    real_rename = (rename_fn)dlsym(RTLD_NEXT, "rename");
  }
}

__attribute__((constructor)) static void unreliable_init(void) {
  resolve_reals();

  const char *seed_raw = getenv("UNRELIABLE_SEED");
  if (seed_raw == NULL || seed_raw[0] == '\0') {
    g_active = 0;
    return;
  }
  g_active = 1;
  g_seed = strtoull(seed_raw, NULL, 10);
  g_powercut = env_u64("UNRELIABLE_POWERCUT", 0) != 0;
  g_eio_ppm = env_u64("UNRELIABLE_EIO_PPM", 0);
  g_short_ppm = env_u64("UNRELIABLE_SHORT_PPM", 0);

  uint64_t explicit_kill = env_u64("UNRELIABLE_KILL_AFTER", 0);
  if (explicit_kill > 0) {
    g_kill_after = explicit_kill;
  } else if (g_powercut) {
    uint64_t max_syscalls = env_u64("UNRELIABLE_MAX_SYSCALLS", 64);
    if (max_syscalls == 0) {
      max_syscalls = 1;
    }
    /* Seed-derived crash point, stable for a given seed. */
    g_kill_after = 1 + (splitmix64(g_seed ^ 0xC0FFEE1234ULL) % max_syscalls);
  } else {
    g_kill_after = 0;
  }
}

/* A regular file we are allowed to fault: fd >= 3 and S_ISREG. */
static int eligible_fd(int fd) {
  if (!g_active || fd < 3) {
    return 0;
  }
  struct stat st;
  if (fstat(fd, &st) != 0) {
    return 0;
  }
  return S_ISREG(st.st_mode) ? 1 : 0;
}

/* SIGKILL self if this eligible-syscall index is the seed-chosen crash point. */
static void maybe_powercut(uint64_t index) {
  if (g_kill_after != 0 && index == g_kill_after) {
    /* Power loss: the pending write never reaches the platter. */
    raise(SIGKILL);
  }
}

static int inject_eio(uint64_t index) {
  if (g_eio_ppm == 0) {
    return 0;
  }
  uint64_t r = splitmix64(g_seed ^ (index * 0x9E3779B97F4A7C15ULL));
  return (r % 1000000ULL) < g_eio_ppm;
}

static int inject_short(uint64_t index) {
  if (g_short_ppm == 0) {
    return 0;
  }
  uint64_t r = splitmix64((g_seed + 0x5BD1E995ULL) ^ (index * 0xD6E8FEB86659FD93ULL));
  return (r % 1000000ULL) < g_short_ppm;
}

ssize_t write(int fd, const void *buf, size_t count) {
  resolve_reals();
  if (!eligible_fd(fd)) {
    return real_write(fd, buf, count);
  }
  uint64_t index = __atomic_add_fetch(&g_counter, 1, __ATOMIC_SEQ_CST);
  maybe_powercut(index);
  if (inject_eio(index)) {
    errno = EIO;
    return -1;
  }
  if (inject_short(index) && count > 1) {
    size_t partial = count / 2;
    if (partial == 0) {
      partial = 1;
    }
    return real_write(fd, buf, partial);
  }
  return real_write(fd, buf, count);
}

ssize_t pwrite(int fd, const void *buf, size_t count, off_t offset) {
  resolve_reals();
  if (!eligible_fd(fd)) {
    return real_pwrite(fd, buf, count, offset);
  }
  uint64_t index = __atomic_add_fetch(&g_counter, 1, __ATOMIC_SEQ_CST);
  maybe_powercut(index);
  if (inject_eio(index)) {
    errno = EIO;
    return -1;
  }
  if (inject_short(index) && count > 1) {
    size_t partial = count / 2;
    if (partial == 0) {
      partial = 1;
    }
    return real_pwrite(fd, buf, partial, offset);
  }
  return real_pwrite(fd, buf, count, offset);
}

/* 64-bit positioned write alias used by some libc/std code paths. */
ssize_t pwrite64(int fd, const void *buf, size_t count, off_t offset) {
  return pwrite(fd, buf, count, offset);
}

int fsync(int fd) {
  resolve_reals();
  if (!eligible_fd(fd)) {
    return real_fsync(fd);
  }
  uint64_t index = __atomic_add_fetch(&g_counter, 1, __ATOMIC_SEQ_CST);
  maybe_powercut(index);
  if (inject_eio(index)) {
    errno = EIO;
    return -1;
  }
  return real_fsync(fd);
}

int fdatasync(int fd) {
  resolve_reals();
  if (!eligible_fd(fd)) {
    return real_fdatasync(fd);
  }
  uint64_t index = __atomic_add_fetch(&g_counter, 1, __ATOMIC_SEQ_CST);
  maybe_powercut(index);
  if (inject_eio(index)) {
    errno = EIO;
    return -1;
  }
  return real_fdatasync(fd);
}

int rename(const char *oldpath, const char *newpath) {
  resolve_reals();
  if (!g_active) {
    return real_rename(oldpath, newpath);
  }
  uint64_t index = __atomic_add_fetch(&g_counter, 1, __ATOMIC_SEQ_CST);
  maybe_powercut(index);
  if (inject_eio(index)) {
    errno = EIO;
    return -1;
  }
  return real_rename(oldpath, newpath);
}
