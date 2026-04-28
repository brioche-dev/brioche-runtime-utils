//! End-to-end tests for brioche-cc.
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo-built brioche-cc binary under test.
const BRIOCHE_CC: &str = env!("CARGO_BIN_EXE_brioche-cc");

/// Inner cc stand-in. Records each argv entry on its own line in
/// `$BRIOCHE_CC_TEST_ARGS` and exits with `$BRIOCHE_CC_TEST_EXIT`
/// (default 0), letting tests inspect what the wrapper passed through.
const RECORDING_INNER_CC: &str = "#!/bin/sh\n\
    set -e\n\
    out_file=\"$BRIOCHE_CC_TEST_ARGS\"\n\
    : > \"$out_file\"\n\
    for arg in \"$@\"; do\n\
        printf '%s\\n' \"$arg\" >> \"$out_file\"\n\
    done\n\
    exit \"${BRIOCHE_CC_TEST_EXIT:-0}\"\n";

/// Lays out the `bin`, `libexec/brioche-cc`, and a real `sysroot` dir
/// the wrapper requires, installs `inner_cc_script` as the inner `cc`,
/// and returns `(wrapper, sysroot)` where `sysroot` is the canonical
/// path the wrapper will inject as `--sysroot`.
fn setup_cc_harness(tmp: &Path, inner_cc_script: &str) -> (PathBuf, PathBuf) {
    let bin = tmp.join("bin");
    let libexec = tmp.join("libexec").join("brioche-cc");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&libexec).unwrap();

    let sysroot = libexec.join("sysroot");
    std::fs::create_dir(&sysroot).unwrap();
    let sysroot = sysroot.canonicalize().unwrap();

    let wrapper = bin.join("cc");
    std::fs::copy(BRIOCHE_CC, &wrapper).unwrap();

    let inner_cc = libexec.join("cc");
    std::fs::write(&inner_cc, inner_cc_script).unwrap();
    let mut perms = std::fs::metadata(&inner_cc).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&inner_cc, perms).unwrap();

    (wrapper, sysroot)
}

/// Reads the recorded argv file written by [`RECORDING_INNER_CC`].
fn read_recorded_args(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// When the user does not pass `--sysroot`, the wrapper injects
/// `--sysroot <path>` before forwarding the rest of argv.
#[test]
fn brioche_cc_injects_sysroot_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let (wrapper, sysroot) = setup_cc_harness(tmp.path(), RECORDING_INNER_CC);
    let args_log = tmp.path().join("args.log");

    let status = Command::new(&wrapper)
        .arg("-c")
        .arg("foo.c")
        .env("BRIOCHE_CC_TEST_ARGS", &args_log)
        .status()
        .expect("run brioche-cc");
    assert!(
        status.success(),
        "brioche-cc exited unsuccessfully: {:?}",
        status.code()
    );

    assert_eq!(
        read_recorded_args(&args_log),
        vec![
            "--sysroot".to_owned(),
            sysroot.to_string_lossy().into_owned(),
            "-c".to_owned(),
            "foo.c".to_owned(),
        ],
    );
}

/// When the user passes their own `--sysroot` (in either spaced or
/// `=`-joined form), the wrapper forwards argv unchanged and does not
/// inject a second `--sysroot`.
#[test]
fn brioche_cc_preserves_user_sysroot() {
    let tmp = tempfile::tempdir().unwrap();
    let (wrapper, _sysroot) = setup_cc_harness(tmp.path(), RECORDING_INNER_CC);
    let args_log = tmp.path().join("args.log");

    let status = Command::new(&wrapper)
        .arg("--sysroot")
        .arg("/custom-sysroot")
        .arg("foo.c")
        .env("BRIOCHE_CC_TEST_ARGS", &args_log)
        .status()
        .expect("run brioche-cc");
    assert!(
        status.success(),
        "brioche-cc exited unsuccessfully: {:?}",
        status.code()
    );
    assert_eq!(
        read_recorded_args(&args_log),
        vec![
            "--sysroot".to_owned(),
            "/custom-sysroot".to_owned(),
            "foo.c".to_owned(),
        ],
    );

    let status = Command::new(&wrapper)
        .arg("--sysroot=/custom-sysroot")
        .arg("foo.c")
        .env("BRIOCHE_CC_TEST_ARGS", &args_log)
        .status()
        .expect("run brioche-cc");
    assert!(
        status.success(),
        "brioche-cc exited unsuccessfully: {:?}",
        status.code()
    );
    assert_eq!(
        read_recorded_args(&args_log),
        vec!["--sysroot=/custom-sysroot".to_owned(), "foo.c".to_owned()],
    );
}

/// The wrapper exec-replaces itself with the inner cc, so the inner
/// cc's exit code surfaces as the wrapper's exit code.
#[test]
fn brioche_cc_forwards_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let (wrapper, _sysroot) = setup_cc_harness(tmp.path(), RECORDING_INNER_CC);
    let args_log = tmp.path().join("args.log");

    let status = Command::new(&wrapper)
        .arg("foo.c")
        .env("BRIOCHE_CC_TEST_ARGS", &args_log)
        .env("BRIOCHE_CC_TEST_EXIT", "7")
        .status()
        .expect("run brioche-cc");
    assert_eq!(status.code(), Some(7));
}
