//! Return free GNU allocator pages when a bounded container approaches its limit.
//! Logical pool reclamation and allocator residency are different: a freed row
//! can leave unused pages in malloc arenas after its budget charge is released.
use std::time::{Duration, Instant};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct AllocatorPressure {
    next_check: Option<Instant>,
}

impl AllocatorPressure {
    pub(super) fn tick(&mut self) {
        self.check(Instant::now(), cgroup_usage, release_unused_pages);
    }

    fn check(
        &mut self,
        now: Instant,
        usage: impl FnOnce() -> Option<(u64, u64)>,
        release: impl FnOnce(u64, u64),
    ) {
        if self.next_check.is_some_and(|next| now < next) {
            return;
        }
        self.next_check = Some(now + CHECK_INTERVAL);
        if let Some((used, limit)) = usage() {
            if limit > 0 && used >= limit - limit / 4 {
                release(used, limit);
            }
        }
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn cgroup_usage() -> Option<(u64, u64)> {
    let membership = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    usage_at(std::path::Path::new("/sys/fs/cgroup"), &membership)
}

#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn usage_at(root: &std::path::Path, membership: &str) -> Option<(u64, u64)> {
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let relative = relative.strip_prefix('/')?;
    if std::path::Path::new(relative)
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let own = root.join(relative);
    read_usage(&own).or_else(|| {
        // A container's cgroup namespace may expose its controller at the
        // mount root rather than the host membership path.
        (own != root).then(|| read_usage(root)).flatten()
    })
}

#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn read_usage(directory: &std::path::Path) -> Option<(u64, u64)> {
    let limit = std::fs::read_to_string(directory.join("memory.max"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    let used = std::fs::read_to_string(directory.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    (limit > 0).then_some((used, limit))
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_unused_pages(used: u64, limit: u64) {
    let started = Instant::now();
    // SAFETY: GNU malloc_trim is thread-safe, takes no pointers and releases
    // only free allocator pages. Live storage and query allocations stay owned.
    let released = unsafe { libc::malloc_trim(0) };
    if released != 0 {
        tracing::debug!(
            used,
            limit,
            elapsed_us = started.elapsed().as_micros(),
            "returned unused allocator pages under cgroup pressure"
        );
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn cgroup_usage() -> Option<(u64, u64)> {
    None
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_unused_pages(_used: u64, _limit: u64) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;

    #[test]
    fn pressure_reclaims_unused_pages_without_polling_on_every_maintenance_tick() {
        let now = Instant::now();
        let releases = Cell::new(0);
        let mut pressure = AllocatorPressure::default();
        pressure.check(now, || Some((75, 100)), |_, _| releases.set(1));
        pressure.check(
            now + Duration::from_millis(200),
            || panic!("the maintenance hot path must not read controller files"),
            |_, _| panic!("allocator reclamation must be paced"),
        );
        pressure.check(
            now + CHECK_INTERVAL,
            || Some((99, 100)),
            |_, _| releases.set(2),
        );
        assert_eq!(releases.get(), 2);
    }

    #[test]
    fn missing_unbounded_or_low_usage_never_requests_reclamation() {
        for usage in [None, Some((74, 100)), Some((0, 0)), Some((1, u64::MAX))] {
            AllocatorPressure::default().check(
                Instant::now(),
                || usage,
                |_, _| panic!("no bounded pressure"),
            );
        }
    }

    #[test]
    fn reads_the_process_controller_and_the_container_namespace_fallback() {
        let temp = tempfile::tempdir().expect("controller fixture");
        let root = temp.path();
        let own = root.join("user.slice/engine.service");
        fs::create_dir_all(&own).expect("membership");
        fs::write(root.join("memory.max"), "max\n").expect("unbounded host");
        fs::write(own.join("memory.max"), "400\n").expect("process limit");
        fs::write(own.join("memory.current"), "350\n").expect("process usage");
        assert_eq!(
            usage_at(root, "0::/user.slice/engine.service\n"),
            Some((350, 400))
        );
        fs::write(root.join("memory.max"), "400\n").expect("namespace limit");
        fs::write(root.join("memory.current"), "300\n").expect("namespace usage");
        assert_eq!(usage_at(root, "0::/missing/host/path\n"), Some((300, 400)));
        assert_eq!(usage_at(root, "0::/\n"), Some((300, 400)));
        assert_eq!(usage_at(root, "0::/../../outside\n"), None);
    }
}
