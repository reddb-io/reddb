use std::path::PathBuf;
use std::process::Command;

#[test]
fn jepsen_black_box_harness_self_test_exercises_replay_artifacts_and_checkers() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("python3")
        .arg(repo.join("scripts/jepsen_black_box_cluster.py"))
        .arg("--self-test")
        .output()
        .expect("run harness self-test");

    assert!(
        output.status.success(),
        "self-test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("seed="), "{stdout}");
    assert!(stdout.contains("history_jsonl="), "{stdout}");
    assert!(stdout.contains("schedule_json="), "{stdout}");
    assert!(stdout.contains("process_kill_restart=true"), "{stdout}");
    assert!(stdout.contains("message_isolation=true"), "{stdout}");
    assert!(
        stdout.contains("committed_write_loss_checker=true"),
        "{stdout}"
    );
    assert!(stdout.contains("stale_leader_checker=true"), "{stdout}");
    assert!(stdout.contains("single_writer_checker=true"), "{stdout}");
}
