//! End-to-end tests for brioche-packer.
use std::io::Read as _;
use std::path::Path;
use std::process::Command;

/// Cargo-built brioche-packer binary under test.
const BRIOCHE_PACKER: &str = env!("CARGO_BIN_EXE_brioche-packer");

/// Minimal `Pack::Static` fixture with two library-dir aliases.
fn static_pack() -> brioche_pack::Pack {
    brioche_pack::Pack::Static {
        library_dirs: vec![b"alias-dir-a".to_vec(), b"alias-dir-b".to_vec()],
    }
}

/// Writes `body` to `path`, then appends `pack` as a brioche-pack trailer.
fn write_packed(path: &Path, body: &[u8], pack: &brioche_pack::Pack) {
    std::fs::write(path, body).unwrap();
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    brioche_pack::inject_pack(&mut file, pack).unwrap();
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

/// `pack` copies the body of `--packed` into `--output` and appends the
/// trailer described by `--pack`. The body bytes survive the copy and the
/// trailer round-trips through `extract_pack`.
#[test]
fn brioche_packer_pack_writes_trailer_onto_body() {
    let tmp = tempfile::tempdir().unwrap();
    let body_path = tmp.path().join("body.bin");
    let body = b"plain-body-bytes";
    std::fs::write(&body_path, body).unwrap();

    let pack = static_pack();
    let pack_json = serde_json::to_string(&pack).unwrap();
    let output = tmp.path().join("packed.bin");

    let status = Command::new(BRIOCHE_PACKER)
        .arg("pack")
        .arg("--packed")
        .arg(&body_path)
        .arg("--output")
        .arg(&output)
        .arg("--pack")
        .arg(&pack_json)
        .status()
        .expect("run brioche-packer pack");
    assert!(
        status.success(),
        "brioche-packer pack exited {:?}",
        status.code()
    );

    let extracted = brioche_pack::extract_pack(std::fs::File::open(&output).unwrap())
        .expect("pack trailer must be present");
    assert_eq!(format!("{:?}", extracted.pack), format!("{:?}", pack));
    assert_eq!(extracted.unpacked_len, body.len());
    assert_eq!(read_prefix(&output, body.len()), body);
}

/// `read` extracts the trailer of an already-packed file and prints it as
/// JSON; deserializing that JSON recovers the original pack.
#[test]
fn brioche_packer_read_emits_pack_as_json() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target.bin");
    let pack = static_pack();
    write_packed(&target, b"unpacked-body-bytes", &pack);

    let output = Command::new(BRIOCHE_PACKER)
        .arg("read")
        .arg(&target)
        .output()
        .expect("run brioche-packer read");
    assert!(
        output.status.success(),
        "brioche-packer read exited {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: brioche_pack::Pack =
        serde_json::from_slice(&output.stdout).expect("read output must be valid Pack JSON");
    assert_eq!(format!("{parsed:?}"), format!("{pack:?}"));
}

/// `autopack --schema` emits the JSON Schema for the autopack config
/// template. Consumers parse it as JSON, so the output must at least be a
/// valid JSON object.
#[test]
fn brioche_packer_autopack_schema_is_valid_json() {
    let output = Command::new(BRIOCHE_PACKER)
        .arg("autopack")
        .arg("--schema")
        .output()
        .expect("run brioche-packer autopack --schema");
    assert!(
        output.status.success(),
        "brioche-packer autopack --schema exited {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("schema output must be valid JSON");
    assert!(value.is_object(), "schema must be a JSON object");
}
