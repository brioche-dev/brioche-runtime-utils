use std::{
    os::unix::fs::OpenOptionsExt as _,
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

    if include_readonly {
        if let Some(input_resource_dirs) = std::env::var_os("BRIOCHE_INPUT_RESOURCE_DIRS") {
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
    }

    match find_resource_dir_from_program(program) {
        Ok(pack_resource_dir) => paths.push(pack_resource_dir),
        Err(PackResourceDirError::NotFound) => {}
        Err(error) => {
            return Err(error);
        }
    }

    if !paths.is_empty() {
        Ok(paths)
    } else {
        Err(PackResourceDirError::NotFound)
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

pub fn find_in_resource_dirs(resource_dirs: &[PathBuf], subpath: &Path) -> Option<PathBuf> {
    for resource_dir in resource_dirs {
        let path = resource_dir.join(subpath);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

fn find_resource_dir_from_program(program: &Path) -> Result<PathBuf, PackResourceDirError> {
    let program = std::env::current_dir()?.join(program);

    let Some(mut current_dir) = program.parent() else {
        return Err(PackResourceDirError::NotFound);
    };

    for _ in 0..SEARCH_DEPTH_LIMIT {
        let pack_resource_dir = current_dir.join("brioche-resources.d");
        if pack_resource_dir.is_dir() {
            return Ok(pack_resource_dir);
        }

        let Some(parent) = current_dir.parent() else {
            return Err(PackResourceDirError::NotFound);
        };

        current_dir = parent;
    }

    Err(PackResourceDirError::DepthLimitReached)
}

pub fn add_named_blob(
    resource_dir: &Path,
    mut contents: impl std::io::Seek + std::io::Read,
    executable: bool,
    name: &Path,
) -> Result<PathBuf, AddBlobError> {
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut contents, &mut hasher)?;
    let hash = hasher.finalize();

    let blob_suffix = if executable { ".x" } else { "" };
    let blob_name = format!("{hash}{blob_suffix}");

    contents.seek(std::io::SeekFrom::Start(0))?;

    let blob_dir = resource_dir.join("blobs");
    let blob_path = blob_dir.join(&blob_name);
    let blob_temp_id = ulid::Ulid::new();
    let blob_temp_path = blob_dir.join(format!("{blob_name}-{blob_temp_id}"));
    std::fs::create_dir_all(&blob_dir)?;

    let mut blob_file_options = std::fs::OpenOptions::new();
    blob_file_options.create_new(true).write(true);
    if executable {
        blob_file_options.mode(0o777);
    }
    let mut blob_file = blob_file_options.open(&blob_temp_path)?;
    std::io::copy(&mut contents, &mut blob_file)?;
    drop(blob_file);
    std::fs::rename(&blob_temp_path, &blob_path)?;

    let alias_dir = resource_dir.join("aliases").join(name).join(&blob_name);
    std::fs::create_dir_all(&alias_dir)?;

    let temp_alias_path = alias_dir.join(format!("{}-{blob_temp_id}", name.display()));
    let alias_path = alias_dir.join(name);
    let blob_pack_relative_path = pathdiff::diff_paths(&blob_path, &alias_dir)
        .expect("blob path is not a prefix of alias path");
    std::os::unix::fs::symlink(blob_pack_relative_path, &temp_alias_path)?;
    std::fs::rename(&temp_alias_path, &alias_path)?;

    let alias_path = alias_path
        .strip_prefix(resource_dir)
        .expect("alias path is not in resource dir");
    Ok(alias_path.to_owned())
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
    #[error("{0}")]
    IoError(#[from] std::io::Error),
}
