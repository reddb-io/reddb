//! Process memory ceilings, including limits inherited from ancestor cgroups.
//! A host service does not necessarily live at the controller mount root.

use std::path::{Component, Path, PathBuf};

/// Separate sources preserve the budget resolver's v2-before-v1 precedence.
pub(crate) fn limits() -> (Option<u64>, Option<u64>) {
    let membership = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    limits_at(Path::new("/sys/fs/cgroup"), &membership)
}

fn limits_at(root: &Path, membership: &str) -> (Option<u64>, Option<u64>) {
    (
        hierarchy_limit(root, membership, "", "memory.max"),
        hierarchy_limit(
            &root.join("memory"),
            membership,
            "memory",
            "memory.limit_in_bytes",
        ),
    )
}

fn hierarchy_limit(root: &Path, membership: &str, controller: &str, file: &str) -> Option<u64> {
    let relative = membership.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        let matches = if controller.is_empty() {
            hierarchy == "0" && controllers.is_empty()
        } else {
            controllers.split(',').any(|name| name == controller)
        };
        matches.then_some(path)
    });
    let relative = relative.unwrap_or("/").strip_prefix('/')?;
    if Path::new(relative)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let mut directory = root.join(relative);
    let mut ceiling = None;
    loop {
        if let Some(limit) = read_limit(&directory.join(file)) {
            ceiling = Some(ceiling.map_or(limit, |old: u64| old.min(limit)));
        }
        if directory == root || !directory.pop() {
            return ceiling;
        }
    }
}

fn read_limit(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes > 0 && *bytes < 0x7FFF_FFFF_FFFF_F000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "reddb-cgroup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("new isolated controller fixture");
            Self(path)
        }

        fn write(&self, path: &str, value: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("controller directories");
            std::fs::write(path, value).expect("controller value");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove this test's fixture");
        }
    }

    #[test]
    fn detects_a_systemd_service_limit_under_an_unbounded_host() {
        let fixture = Fixture::new();
        fixture.write("memory.max", "max\n");
        fixture.write("user.slice/engine.service/memory.max", "536870912\n");
        assert_eq!(
            limits_at(&fixture.0, "0::/user.slice/engine.service\n"),
            (Some(536870912), None)
        );
    }

    #[test]
    fn inherited_limit_wins_over_a_larger_or_unlimited_child() {
        let fixture = Fixture::new();
        fixture.write("memory.max", "2147483648");
        fixture.write("tenant/memory.max", "268435456");
        for child in ["max", "1073741824"] {
            fixture.write("tenant/engine/memory.max", child);
            assert_eq!(
                limits_at(&fixture.0, "0::/tenant/engine\n").0,
                Some(268435456)
            );
        }
    }

    #[test]
    fn namespaced_mount_root_remains_a_supported_source() {
        let fixture = Fixture::new();
        fixture.write("memory.max", "536870912");
        for membership in ["0::/\n", "0::/host/hidden/container\n", ""] {
            assert_eq!(limits_at(&fixture.0, membership).0, Some(536870912));
        }
    }

    #[test]
    fn v1_uses_memory_membership_and_inherited_ceiling() {
        let fixture = Fixture::new();
        fixture.write("memory/memory.limit_in_bytes", "9223372036854771712");
        fixture.write("memory/tenant/memory.limit_in_bytes", "536870912");
        fixture.write("memory/tenant/engine/memory.limit_in_bytes", "1073741824");
        assert_eq!(
            limits_at(
                &fixture.0,
                "3:cpu:/elsewhere\n5:memory,other:/tenant/engine\n"
            ),
            (None, Some(536870912))
        );
    }

    #[test]
    fn absent_invalid_and_unlimited_limits_do_not_become_budgets() {
        let fixture = Fixture::new();
        for value in ["max", "", "garbage", "0", "9223372036854771712"] {
            fixture.write("memory.max", value);
            fixture.write("memory/memory.limit_in_bytes", value);
            assert_eq!(limits_at(&fixture.0, "0::/\n"), (None, None));
        }
    }

    #[test]
    fn malformed_membership_cannot_escape_controller_root() {
        let fixture = Fixture::new();
        fixture.write("outside/memory.max", "123");
        fixture.write("controller/memory.max", "max");
        assert_eq!(
            limits_at(&fixture.0.join("controller"), "0::/../outside\n"),
            (None, None)
        );
    }
}
