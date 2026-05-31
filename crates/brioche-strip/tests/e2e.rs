//! End-to-end tests for brioche-strip.
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cargo-built brioche-strip binary under test.
const BRIOCHE_STRIP: &str = env!("CARGO_BIN_EXE_brioche-strip");

/// Inner strip stand-in. Truncates each positional arg down to
/// [`MARKER_BODY`], proving the wrapper invoked strip on its remapped
/// temp file rather than no-oping.
const MARKER_INNER_STRIP: &str = "#!/bin/sh\n\
    set -e\n\
    for arg in \"$@\"; do\n\
        case \"$arg\" in\n\
            -*) ;;\n\
            *) printf 'STRIPPED' > \"$arg\" ;;\n\
        esac\n\
    done\n";

/// Bytes [`MARKER_INNER_STRIP`] writes to its target file.
const MARKER_BODY: &[u8] = b"STRIPPED";

/// Lays out the `bin` and `libexec/brioche-strip` siblings the wrapper
/// expects, installs `inner_strip_script` as the inner `strip`, and
/// returns the wrapper path callers should invoke.
fn setup_strip_harness(tmp: &Path, inner_strip_script: &str) -> PathBuf {
    let bin = tmp.join("bin");
    let libexec = tmp.join("libexec").join("brioche-strip");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&libexec).unwrap();

    let wrapper = bin.join("brioche-strip");
    std::fs::hard_link(BRIOCHE_STRIP, &wrapper).unwrap();

    let inner_strip = libexec.join("strip");
    std::fs::write(&inner_strip, inner_strip_script).unwrap();
    let mut perms = std::fs::metadata(&inner_strip).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&inner_strip, perms).unwrap();

    wrapper
}

/// Writes `body` to `path`, then appends `pack` as a brioche-pack trailer.
fn write_packed(path: &Path, body: &[u8], pack: &brioche_pack::Pack) {
    std::fs::write(path, body).unwrap();
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    brioche_pack::inject_pack(&mut file, pack).unwrap();
}

/// Minimal `Pack::Static` fixture with two library-dir aliases.
fn static_pack() -> brioche_pack::Pack {
    brioche_pack::Pack::Static {
        library_dirs: vec![b"alias-dir-a".to_vec(), b"alias-dir-b".to_vec()],
    }
}

/// Reads the first `len` bytes of `path`.
fn read_prefix(path: &Path, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    std::fs::File::open(path)
        .unwrap()
        .read_exact(&mut buf)
        .unwrap();
    buf
}

/// In-place strip preserves the pack trailer: the wrapper hands the
/// unpacked body to the inner strip and re-emits the file with the
/// original trailer on top of strip's output.
#[test]
fn brioche_strip_preserves_pack_trailer_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = setup_strip_harness(tmp.path(), MARKER_INNER_STRIP);

    let target = tmp.path().join("target.bin");
    let pack = static_pack();
    write_packed(&target, b"unpacked-body-bytes", &pack);

    let resource_dir = tmp.path().join("brioche-resources.d");
    std::fs::create_dir(&resource_dir).unwrap();

    let status = Command::new(&wrapper)
        .arg(&target)
        .env("BRIOCHE_RESOURCE_DIR", &resource_dir)
        .status()
        .expect("run brioche-strip");
    assert!(
        status.success(),
        "brioche-strip exited unsuccessfully: {:?}",
        status.code()
    );

    let extracted = brioche_pack::extract_pack(std::fs::File::open(&target).unwrap())
        .expect("pack trailer must still be present after strip");
    assert_eq!(format!("{:?}", extracted.pack), format!("{:?}", pack));

    assert_eq!(extracted.unpacked_len, MARKER_BODY.len());
    assert_eq!(read_prefix(&target, MARKER_BODY.len()), MARKER_BODY);
}

/// Strip with `-o` preserves the pack trailer on the output and leaves
/// the input file untouched.
#[test]
fn brioche_strip_preserves_pack_trailer_with_output_arg() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = setup_strip_harness(tmp.path(), MARKER_INNER_STRIP);

    let input = tmp.path().join("input.bin");
    let pack = static_pack();
    write_packed(&input, b"unpacked-body-bytes", &pack);
    let original_input = std::fs::read(&input).unwrap();

    let resource_dir = tmp.path().join("brioche-resources.d");
    std::fs::create_dir(&resource_dir).unwrap();
    let output = tmp.path().join("output.bin");

    let status = Command::new(&wrapper)
        .arg("-o")
        .arg(&output)
        .arg(&input)
        .env("BRIOCHE_RESOURCE_DIR", &resource_dir)
        .status()
        .expect("run brioche-strip");
    assert!(
        status.success(),
        "brioche-strip exited unsuccessfully: {:?}",
        status.code()
    );

    assert_eq!(std::fs::read(&input).unwrap(), original_input);

    let extracted = brioche_pack::extract_pack(std::fs::File::open(&output).unwrap())
        .expect("pack trailer must be present in output");
    assert_eq!(format!("{:?}", extracted.pack), format!("{:?}", pack));

    assert_eq!(extracted.unpacked_len, MARKER_BODY.len());
    assert_eq!(read_prefix(&output, MARKER_BODY.len()), MARKER_BODY);
}

/// Pass-through mode bypasses pack handling and propagates the inner
/// strip's exit code unchanged. The target file's contents do not
/// matter; nothing reads them.
#[test]
fn brioche_strip_passthrough_forwards_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let wrapper = setup_strip_harness(tmp.path(), "#!/bin/sh\nexit 7\n");

    let target = tmp.path().join("target.bin");
    std::fs::write(&target, b"plain-content").unwrap();

    let status = Command::new(&wrapper)
        .arg(&target)
        .env("BRIOCHE_STRIP_AUTOPACK", "false")
        .status()
        .expect("run brioche-strip");
    assert_eq!(status.code(), Some(7));
}
