use std::{
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use bstr::ByteSlice as _;

const SEARCH_DEPTH_LIMIT: u32 = 64;

pub fn find_resource_dirs(
    program: &Path,
    include_readonly: bool,
) -> Result<Vec<PathBuf>, PackResourceDirError> {
    let mut paths = vec![];
    if let Some(pack_resource_dir) = std::env::var_os("BRIOCHE_RESOURCE_DIR") {
        paths.push(PathBuf::from(pack_resource_dir));
    }

    if include_readonly
        && let Some(input_resource_dirs) = std::env::var_os("BRIOCHE_INPUT_RESOURCE_DIRS")
    {
        if let Some(input_resource_dirs) = <[u8]>::from_os_str(&input_resource_dirs) {
            for input_resource_dir in input_resource_dirs.split_str(b":") {
                if let Ok(path) = input_resource_dir.to_path() {
                    paths.push(path.to_owned());
                }
            }
        }

        for input_resource_dir in std::env::split_paths(&input_resource_dirs) {
            paths.push(input_resource_dir);
        }
    }

    match find_resource_dirs_from_program(program, &mut paths) {
        Ok(()) | Err(PackResourceDirError::NotFound) => {}
        Err(error) => {
            return Err(error);
        }
    }

    if paths.is_empty() {
        Err(PackResourceDirError::NotFound)
    } else {
        Ok(paths)
    }
}

pub fn find_output_resource_dir(program: &Path) -> Result<PathBuf, PackResourceDirError> {
    let resource_dirs = find_resource_dirs(program, false)?;
    let resource_dir = resource_dirs
        .into_iter()
        .next()
        .ok_or(PackResourceDirError::NotFound)?;
    Ok(resource_dir)
}

#[must_use]
pub fn find_in_resource_dirs(resource_dirs: &[PathBuf], subpath: &Path) -> Option<PathBuf> {
    for resource_dir in resource_dirs {
        let path = resource_dir.join(subpath);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn find_resource_dirs_from_program(
    program: &Path,
    resource_dirs: &mut Vec<PathBuf>,
) -> Result<(), PackResourceDirError> {
    let program = std::env::current_dir()?.join(program);

    let Some(mut current_dir) = program.parent() else {
        return Err(PackResourceDirError::NotFound);
    };

    let mut found = false;
    let mut reached_end = false;
    for _ in 0..SEARCH_DEPTH_LIMIT {
        let pack_resource_dir = current_dir.join("brioche-resources.d");
        if pack_resource_dir.is_dir() {
            resource_dirs.push(pack_resource_dir);
            found = true;
        }

        let Some(parent) = current_dir.parent() else {
            reached_end = true;
            break;
        };

        current_dir = parent;
    }

    if found {
        Ok(())
    } else if reached_end {
        Err(PackResourceDirError::NotFound)
    } else {
        Err(PackResourceDirError::DepthLimitReached)
    }
}

pub fn add_named_blob(
    resource_dir: &Path,
    mut contents: impl std::io::BufRead,
    executable: bool,
    name: &Path,
) -> Result<PathBuf, AddBlobError> {
    // Create the 'blobs' directory
    let blob_dir = resource_dir.join("blobs");
    std::fs::create_dir_all(&blob_dir)?;

    // Open a temporary file to copy the contents to
    let blob_temp_id = ulid::Ulid::new();
    let blob_temp_path = blob_dir.join(blob_temp_id.to_string());
    let mut blob_file_options = std::fs::OpenOptions::new();
    blob_file_options.create_new(true).write(true);
    if executable {
        blob_file_options.mode(0o777);
    }
    let blob_file = blob_file_options.open(&blob_temp_path)?;
    let mut blob_file = std::io::BufWriter::new(blob_file);

    // Read the contents, both copying it to the temporary file and hashing
    // as we go
    let mut hasher = blake3::Hasher::new();
    loop {
        let buf = contents.fill_buf()?;
        if buf.is_empty() {
            break;
        }

        hasher.update(buf);
        blob_file.write_all(buf)?;

        let consumed = buf.len();
        contents.consume(consumed);
    }

    // Get the hash of the contents, which we'll use as the blob's name
    let hash = hasher.finalize();

    // Get the final blob's filename. We use a suffix to distinguish identical
    // blobs with different permissions
    let blob_suffix = if executable { ".x" } else { "" };
    let blob_name = format!("{hash}{blob_suffix}");
    let blob_path = blob_dir.join(&blob_name);

    // Rename the blob to its final path
    drop(blob_file);
    std::fs::rename(&blob_temp_path, &blob_path)?;

    // Create a temporary directory for the alias dir
    let alias_temp_dir = resource_dir.join(format!("{blob_name}-{blob_temp_id}-alias"));
    let alias_temp_path = alias_temp_dir.join(name);
    std::fs::create_dir(&alias_temp_dir)?;

    // Create the symlink within the temporary dir
    let alias_parent_dir = resource_dir.join("aliases").join(name);
    let alias_dir = alias_parent_dir.join(&blob_name);
    let blob_pack_relative_path = pathdiff::diff_paths(&blob_path, &alias_dir)
        .expect("blob path is not a prefix of alias path");
    std::os::unix::fs::symlink(&blob_pack_relative_path, &alias_temp_path)?;

    // Create directory for the alias dir
    std::fs::create_dir_all(&alias_parent_dir)?;

    // Rename the temp dir to the final alias path. This ensures that the alias
    // dir itself is atomic, and never appears empty
    let alias_path = alias_dir.join(name);
    let result = std::fs::rename(&alias_temp_dir, alias_dir);
    match result {
        Ok(()) => {
            // Alias dir created successfully
        }
        Err(err)
            if err.kind() == std::io::ErrorKind::AlreadyExists
                || err.kind() == std::io::ErrorKind::DirectoryNotEmpty =>
        {
            // Could not rename temp alias dir to final path. On Unix, this
            // means that the alias dir already exists and is non-empty

            // Clean up the temporary dir first
            std::fs::remove_dir_all(&alias_temp_dir)?;

            // Try to create the symlink again-- this time in its final path
            let result = std::os::unix::fs::symlink(&blob_pack_relative_path, &alias_path);
            match result {
                Ok(()) => {
                    // Symlink created successfully. This means the alias
                    // dir already existed and was not empty, but contained
                    // something else? This probably shouldn't happen...
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Path already exists, nothing to do
                }
                Err(err) => {
                    return Err(err.into());
                }
            }
        }
        Err(err) => {
            return Err(err.into());
        }
    }

    // Return the symlink alias path relative to the resource dir
    let alias_path = alias_path
        .strip_prefix(resource_dir)
        .expect("alias path is not in resource dir");
    Ok(alias_path.to_owned())
}

pub fn add_named_resource_directory(
    resource_dir: &Path,
    source: &Path,
    hint_name: &str,
) -> Result<PathBuf, AddNamedDirectoryError> {
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
        .expect("hashed path is not a prefix of alias path");
    std::os::unix::fs::symlink(hashed_relative_path, &alias_path)?;

    let alias_path = alias_path
        .strip_prefix(resource_dir)
        .expect("alias path not in resource dir");
    Ok(alias_path.to_owned())
}

fn hash_directory(path: &Path) -> Result<blake3::Hash, std::io::Error> {
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

#[derive(Debug, thiserror::Error)]
pub enum PackResourceDirError {
    #[error("brioche pack resource dir not found")]
    NotFound,
    #[error("error while searching for brioche pack resource dir: {0}")]
    IoError(#[from] std::io::Error),
    #[error("reached depth limit while searching for brioche pack resource dir")]
    DepthLimitReached,
}

#[derive(Debug, thiserror::Error)]
pub enum AddBlobError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum AddNamedDirectoryError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
