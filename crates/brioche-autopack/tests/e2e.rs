//! End-to-end tests for brioche-autopack.
#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Compiles `src` to `output` with the system gcc, forwarding `extra`
/// flags ahead of the input.
fn compile_c(src: &Path, output: &Path, extra: &[&str]) {
    let status = Command::new("gcc")
        .args(extra)
        .arg(src)
        .arg("-o")
        .arg(output)
        .status()
        .expect("run gcc");
    assert!(status.success(), "gcc exited {:?}", status.code());
}

/// A shared library passed through autopack gets its pack trailer
/// appended past the ELF segments, so a well-behaved dynamic loader
/// dlopens it and resolves its symbols normally.
#[test]
fn autopack_shared_library_remains_dlopen_able() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("addlib.c");
    std::fs::write(
        &src,
        "int brioche_test_add(int a, int b) { return a + b; }\n",
    )
    .unwrap();

    let lib = tmp.path().join("libbriochetest.so");
    compile_c(&src, &lib, &["-shared", "-fPIC"]);

    let resource_dir = tmp.path().join("brioche-resources.d");
    std::fs::create_dir(&resource_dir).unwrap();

    let dynamic_linking = brioche_autopack::DynamicLinkingConfig {
        library_paths: library_search_dirs(),
        skip_libraries: HashSet::new(),
        extra_libraries: vec![],
        skip_unknown_libraries: true,
    };
    brioche_autopack::autopack(
        brioche_autopack::AutopackInputs::Paths(vec![lib.clone()]),
        &brioche_autopack::AutopackConfig {
            resource_dir: resource_dir.clone(),
            all_resource_dirs: vec![resource_dir],
            quiet: true,
            link_dependencies: vec![PathBuf::from("/")],
            dynamic_binary: None,
            shared_library: Some(brioche_autopack::SharedLibraryConfig {
                dynamic_linking,
                allow_empty: true,
            }),
            script: None,
            repack: None,
        },
    )
    .expect("autopack");

    brioche_pack::extract_pack(std::fs::File::open(&lib).unwrap())
        .expect("autopack must inject pack trailer");

    // SAFETY: the loaded library defines a pure, well-typed function with no
    // global state; calling it is safe.
    unsafe {
        let library = libloading::Library::new(&lib).expect("dlopen");
        let func: libloading::Symbol<unsafe extern "C" fn(i32, i32) -> i32> = library
            .get(b"brioche_test_add\0")
            .expect("symbol brioche_test_add");
        assert_eq!(func(2, 3), 5);
        assert_eq!(func(-7, 7), 0);
    }
}
