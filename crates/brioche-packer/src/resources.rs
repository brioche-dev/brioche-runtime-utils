use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use eyre::OptionExt as _;

pub fn add_named_resource_directory(
    resource_dir: &Path,
    source: &Path,
    hint_name: &str,
) -> eyre::Result<PathBuf> {
    let resources_directories_dir = resource_dir.join("directories");
    std::fs::create_dir_all(&resources_directories_dir)?;

    let temp_name = ulid::Ulid::new().to_string();
    let temp_path = resources_directories_dir.join(temp_name);
    copy_dir::copy_dir(source, &temp_path)?;

    let directory_hash = hash_directory(&temp_path)?;
    let directory_name = format!("{directory_hash}.d");
    let hashed_path = resources_directories_dir.join(&directory_name);
    std::fs::rename(&temp_path, &hashed_path)?;

    let alias_dir = resource_dir.join("aliases").join(hint_name);
    std::fs::create_dir_all(&alias_dir)?;
    let alias_path = alias_dir.join(&directory_name);

    let hashed_relative_path = pathdiff::diff_paths(hashed_path, &alias_dir)
        .ok_or_eyre("hashed path is not a prefix of alias path")?;
    std::os::unix::fs::symlink(hashed_relative_path, &alias_path)?;

    let alias_path = alias_path
        .strip_prefix(resource_dir)
        .expect("alias path not in resource dir");
    Ok(alias_path.to_owned())
}

fn hash_directory(path: &Path) -> eyre::Result<blake3::Hash> {
    let walkdir = walkdir::WalkDir::new(path).sort_by_file_name();
    let mut hasher = blake3::Hasher::new();

    for entry in walkdir {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = entry.metadata()?;
        let file_type = metadata.file_type();
        let entry_path_encoded = entry_path.as_os_str().as_encoded_bytes();
        let entry_path_encoded = tick_encoding::encode(entry_path_encoded);

        if file_type.is_file() {
            let file_len = metadata.len();
            let permissions = metadata.permissions();
            let mode = permissions.mode();
            let is_executable = mode & 0o111 != 0;
            let mut file = std::fs::File::open(path.join(entry_path))?;

            writeln!(hasher, "f:{entry_path_encoded}:{file_len}:{is_executable}")?;
            std::io::copy(&mut file, &mut hasher)?;
        } else if file_type.is_dir() {
            writeln!(hasher, "d:{entry_path_encoded}")?;
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(path.join(entry_path))?;
            let target = target.as_os_str().as_encoded_bytes();
            let target = tick_encoding::encode(target);
            let target_len = target.len();
            writeln!(hasher, "s:{entry_path_encoded}:{target_len}")?;
            hasher.write_all(target.as_bytes())?;
        }
    }

    let hash = hasher.finalize();
    Ok(hash)
}
