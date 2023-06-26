#[cfg(test)]
use assert_cmd::Command;

// Integration tests that buckle can download buck2 and run it with same arguments.
#[test]
fn test_buck2_latest() {
    let tmpdir = tempfile::TempDir::new().unwrap();
    let mut cmd = Command::cargo_bin("buckle").unwrap();
    cmd.env("BUCKLE_HOME", tmpdir.path().as_os_str());
    cmd.arg("--version");
    let assert = cmd.assert();
    let stderr = String::from_utf8(assert.get_output().stderr.to_vec()).unwrap();
    assert!(stderr.contains("/buckle/buck2/"), "found {}", stderr);
    let stdout = String::from_utf8(assert.get_output().stdout.to_vec()).unwrap();
    assert!(stdout.starts_with("buck2 "), "found {}", stdout);
    assert.success();
}

#[test]
fn test_buck2_fail() {
    let mut cmd = Command::cargo_bin("buckle").unwrap();
    cmd.arg("--totally-unknown-argument");
    let assert = cmd.assert();
    let stderr = String::from_utf8(assert.get_output().stderr.to_vec()).unwrap();
    assert!(stderr.contains("/buckle/buck2/"), "found {}", stderr);
    assert.failure();
}
