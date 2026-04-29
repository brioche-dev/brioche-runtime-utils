//! End-to-end tests for brioche-packed-plain-exec.
#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo-built brioche-packed-plain-exec binary under test.
const PACKED_PLAIN_EXEC: &str = env!("CARGO_BIN_EXE_brioche-packed-plain-exec");

/// System library directories likely to hold libc and ld-linux. Filtered
/// to existing dirs so non-multiarch hosts still resolve a usable subset.
fn library_search_dirs() -> Vec<PathBuf> {
    [
        "/lib/x86_64-linux-gnu",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib64",
        "/usr/lib64",
        "/lib",
        "/usr/lib",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|p| p.is_dir())
    .collect()
}

/// Compiles `src` to `output` with the system gcc.
fn compile_c(src: &Path, output: &Path) {
    let status = Command::new("gcc")
        .arg(src)
        .arg("-o")
        .arg(output)
        .status()
        .expect("run gcc");
    assert!(status.success(), "gcc exited {:?}", status.code());
}

/// Lays out the `bin` and sibling `brioche-resources.d` the runtime's
/// directory walk requires, and returns `(output_path, resource_dir)`.
fn setup_packed_layout(tmp: &Path, exe_name: &str) -> (PathBuf, PathBuf) {
    let root = tmp.join("root");
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let resource_dir = root.join("brioche-resources.d");
    std::fs::create_dir(&resource_dir).unwrap();
    (bin_dir.join(exe_name), resource_dir)
}

/// Autopacks `binary` in place as a dynamic executable that defers to the
/// brioche-packed-plain-exec runtime under test, then restores exec bits
/// (autopack writes via `File::create`, which honors umask).
fn autopack_dynamic(binary: &Path, resource_dir: &Path) {
    let dynamic_linking = brioche_autopack::DynamicLinkingConfig {
        library_paths: library_search_dirs(),
        skip_libraries: HashSet::new(),
        extra_libraries: vec![],
        skip_unknown_libraries: true,
    };
    brioche_autopack::autopack(
        brioche_autopack::AutopackInputs::Paths(vec![binary.to_path_buf()]),
        &brioche_autopack::AutopackConfig {
            resource_dir: resource_dir.to_path_buf(),
            all_resource_dirs: vec![resource_dir.to_path_buf()],
            quiet: true,
            link_dependencies: vec![PathBuf::from("/")],
            dynamic_binary: Some(brioche_autopack::DynamicBinaryConfig {
                packed_executable: PathBuf::from(PACKED_PLAIN_EXEC),
                extra_runtime_library_paths: vec![],
                dynamic_linking,
            }),
            shared_library: None,
            script: None,
            repack: None,
        },
    )
    .expect("autopack");

    let mut perms = std::fs::metadata(binary).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(binary, perms).unwrap();
}

/// A dynamic binary packed via autopack, when invoked, runs through the
/// brioche-packed-plain-exec runtime: stdout from the underlying program
/// is preserved and its exit code surfaces as the packed exit code.
#[test]
fn packed_dynamic_binary_executes_through_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("hello.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n\
         int main(void){ printf(\"hello-from-packed\\n\"); return 17; }\n",
    )
    .unwrap();

    let (output, resource_dir) = setup_packed_layout(tmp.path(), "hello");
    compile_c(&src, &output);
    autopack_dynamic(&output, &resource_dir);

    let run = Command::new(&output).output().expect("run packed");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert_eq!(
        run.status.code(),
        Some(17),
        "exit code mismatch; stdout={stdout} stderr={stderr}",
    );
    assert!(stdout.contains("hello-from-packed"), "stdout={stdout}");
}
