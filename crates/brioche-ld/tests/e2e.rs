//! End-to-end tests for brioche-ld.
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo-built brioche-ld binary under test.
const BRIOCHE_LD: &str = env!("CARGO_BIN_EXE_brioche-ld");

/// Inner ld stand-in for pass-through paths. Writes [`MARKER_BODY`]
/// to its `-o` target, proving the wrapper invoked it.
const MARKER_INNER_LD: &str = "#!/bin/sh\n\
    set -e\n\
    while [ $# -gt 0 ]; do\n\
        case \"$1\" in\n\
            -o) shift; printf 'LINKED' > \"$1\"; shift ;;\n\
            -o*) printf 'LINKED' > \"${1#-o}\"; shift ;;\n\
            *) shift ;;\n\
        esac\n\
    done\n";

/// Bytes [`MARKER_INNER_LD`] writes to its target.
const MARKER_BODY: &[u8] = b"LINKED";

/// Lays out the `bin` and `libexec/brioche-ld` siblings the wrapper
/// expects, installs `inner_ld_script` as the inner `ld`, and returns
/// the wrapper path callers should invoke.
fn setup_ld_harness(tmp: &Path, inner_ld_script: &str) -> PathBuf {
    let bin = tmp.join("bin");
    let libexec = tmp.join("libexec").join("brioche-ld");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&libexec).unwrap();

    let wrapper = bin.join("ld");
    std::fs::hard_link(BRIOCHE_LD, &wrapper).unwrap();

    let inner_ld = libexec.join("ld");
    std::fs::write(&inner_ld, inner_ld_script).unwrap();
    let mut perms = std::fs::metadata(&inner_ld).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&inner_ld, perms).unwrap();

    wrapper
}

/// System library directories likely to hold libc and ld-linux. Filtered
/// to existing dirs so non-multiarch hosts still resolve a usable subset.
#[cfg(target_os = "linux")]
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

/// Pass-through mode skips autopack: the wrapper invokes the inner
/// linker and leaves its output bytes untouched.
#[test]
fn brioche_ld_passthrough_preserves_inner_output() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = setup_ld_harness(tmp.path(), MARKER_INNER_LD);

    let output = tmp.path().join("a.out");

    let status = Command::new(&wrapper)
        .arg("-o")
        .arg(&output)
        .arg("dummy.o")
        .env("BRIOCHE_LD_AUTOPACK", "false")
        .status()
        .expect("run brioche-ld");
    assert!(
        status.success(),
        "brioche-ld exited unsuccessfully: {:?}",
        status.code()
    );

    assert_eq!(std::fs::read(&output).unwrap(), MARKER_BODY);
}

/// Pass-through mode propagates the inner linker's exit code unchanged.
#[test]
fn brioche_ld_passthrough_forwards_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = setup_ld_harness(tmp.path(), "#!/bin/sh\nexit 7\n");

    let output = tmp.path().join("a.out");

    let status = Command::new(&wrapper)
        .arg("-o")
        .arg(&output)
        .arg("dummy.o")
        .env("BRIOCHE_LD_AUTOPACK", "false")
        .status()
        .expect("run brioche-ld");
    assert_eq!(status.code(), Some(7));
}

/// The wrapper invokes the inner linker, which emits a real ELF,
/// then autopacks the output so its trailer extracts as an `LdLinux`
/// pack.
#[cfg(target_os = "linux")]
#[test]
fn brioche_ld_autopacks_its_output() {
    let target_bin_dir = Path::new(BRIOCHE_LD).parent().expect("brioche-ld parent");
    let packed_runtime = target_bin_dir.join("brioche-packed-plain-exec");
    assert!(
        packed_runtime.is_file(),
        "brioche-packed-plain-exec missing at {} (run cargo test --workspace)",
        packed_runtime.display(),
    );

    let tmp = tempfile::tempdir().unwrap();

    let src = tmp.path().join("hello.c");
    std::fs::write(&src, "int main(void){return 0;}\n").unwrap();
    let prebuilt = tmp.path().join("prebuilt");
    let gcc_status = Command::new("gcc")
        .arg(&src)
        .arg("-o")
        .arg(&prebuilt)
        .status()
        .expect("run gcc");
    assert!(gcc_status.success(), "gcc exited {:?}", gcc_status.code());

    let inner_ld_script = format!(
        "#!/bin/sh\n\
         set -e\n\
         out=\n\
         while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
                 -o) shift; out=\"$1\"; shift ;;\n\
                 -o*) out=\"${{1#-o}}\"; shift ;;\n\
                 *) shift ;;\n\
             esac\n\
         done\n\
         if [ -z \"$out\" ]; then exit 1; fi\n\
         cp \"{}\" \"$out\"\n",
        prebuilt.to_string_lossy(),
    );
    let wrapper = setup_ld_harness(tmp.path(), &inner_ld_script);

    let ld_resource_dir = tmp.path().join("libexec").join("brioche-ld");
    let runtime_link = ld_resource_dir.join("brioche-packed");
    std::os::unix::fs::symlink(&packed_runtime, &runtime_link).unwrap();

    let prebuilt_bytes = std::fs::read(&prebuilt).unwrap();
    let elf = goblin::elf::Elf::parse(&prebuilt_bytes).expect("parse prebuilt ELF");
    let interpreter = elf
        .interpreter
        .expect("prebuilt must have a dynamic interpreter");
    let rel_interpreter = Path::new(interpreter)
        .strip_prefix("/")
        .expect("interpreter path must be absolute");
    let staged_interpreter = ld_resource_dir.join(rel_interpreter);
    std::fs::create_dir_all(staged_interpreter.parent().unwrap()).unwrap();
    std::fs::copy(interpreter, &staged_interpreter).unwrap();

    let output_bin = tmp.path().join("output").join("bin");
    std::fs::create_dir_all(&output_bin).unwrap();
    std::fs::create_dir(tmp.path().join("brioche-resources.d")).unwrap();
    let output = output_bin.join("hello");

    let mut command = Command::new(&wrapper);
    command.arg("-o").arg(&output);
    for lib_dir in library_search_dirs() {
        command.arg("-L").arg(lib_dir);
    }
    command.arg("dummy.o");
    let status = command.status().expect("run brioche-ld");
    assert!(
        status.success(),
        "brioche-ld exited unsuccessfully: {:?}",
        status.code()
    );

    let extracted = brioche_pack::extract_pack(std::fs::File::open(&output).unwrap())
        .expect("pack trailer must be present in output");
    assert!(
        matches!(extracted.pack, brioche_pack::Pack::LdLinux { .. }),
        "expected LdLinux pack, got {:?}",
        extracted.pack,
    );
}
