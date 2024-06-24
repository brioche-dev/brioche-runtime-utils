use std::{
    collections::{HashSet, VecDeque},
    io::Read as _,
    path::{Path, PathBuf},
};

use bstr::{ByteSlice as _, ByteVec as _};
use eyre::{Context as _, OptionExt as _};

#[derive(Debug, Clone, clap::Parser)]
pub struct AutowrapArgs {
    recipe_path: PathBuf,
    #[arg(long)]
    quiet: bool,
    #[arg(long = "path")]
    paths: Vec<String>,
    #[arg(long = "glob")]
    globs: Vec<String>,
    #[arg(long = "link-dependency")]
    link_dependencies: Vec<PathBuf>,
    #[arg(long = "self-dependency")]
    self_dependency: bool,
    #[command(flatten)]
    dynamic_linking_args: DynamicLinkingArgs,
    #[command(flatten)]
    dynamic_binary_args: DynamicBinaryArgs,
    #[command(flatten)]
    shared_library_args: SharedLibraryArgs,
    #[command(flatten)]
    script_args: ScriptArgs,
    #[command(flatten)]
    rewrap_args: RewrapArgs,
}

#[derive(Debug, Clone, clap::Parser)]
struct DynamicLinkingArgs {
    #[arg(long = "dynamic-linking-skip-library")]
    skip_libraries: Vec<String>,
    #[arg(long = "dynamic-linking-skip-unknown-libraries")]
    skip_unknown_libraries: bool,
}

#[derive(Debug, Clone, clap::Parser)]
struct DynamicBinaryArgs {
    #[arg(long = "dynamic-binary-enable")]
    dynamic_binary_enable: bool,
    #[arg(long = "dynamic-binary-packed-executable")]
    dynamic_binary_packed_executable: PathBuf,
    #[arg(long = "dynamic-binary-extra-library")]
    extra_libraries: Vec<String>,
}

#[derive(Debug, Clone, clap::Parser)]
struct SharedLibraryArgs {
    #[arg(long = "shared-library-enable")]
    shared_library_enable: bool,
}

#[derive(Debug, Clone, clap::Parser)]
struct ScriptArgs {
    #[arg(long = "script-enable")]
    script_enable: bool,
    #[arg(long = "script-packed-executable")]
    script_packed_executable: PathBuf,
}

#[derive(Debug, Clone, clap::Parser)]
struct RewrapArgs {
    #[arg(long = "rewrap-enable")]
    rewrap_enable: bool,
}

pub fn autowrap(args: &AutowrapArgs) -> eyre::Result<()> {
    let ctx = autowrap_context(args)?;

    for path in &args.paths {
        let path = args.recipe_path.join(path);
        let did_wrap = try_autowrap_path(&ctx, &path)?;
        eyre::ensure!(did_wrap, "failed to wrap path: {path:?}");
        if !args.quiet {
            println!("wrapped {}", path.display());
        }
    }

    let mut globs = globset::GlobSetBuilder::new();
    for glob in &args.globs {
        globs.add(globset::Glob::new(glob)?);
    }

    let globs = globs.build()?;

    let walkdir = walkdir::WalkDir::new(&args.recipe_path);
    for entry in walkdir {
        let entry = entry?;
        if globs.is_match(entry.path()) {
            let did_wrap = try_autowrap_path(&ctx, entry.path())?;
            if !args.quiet {
                if did_wrap {
                    println!("wrapped {}", entry.path().display());
                } else {
                    println!("skipped {}", entry.path().display());
                }
            }
        }
    }

    Ok(())
}

struct AutowrapContext<'a> {
    args: &'a AutowrapArgs,
    resource_dir: PathBuf,
    all_resource_dirs: Vec<PathBuf>,
    link_dependencies: Vec<PathBuf>,
    link_dependency_library_paths: Vec<PathBuf>,
    skip_libraries: HashSet<&'a str>,
}

fn autowrap_context(args: &AutowrapArgs) -> eyre::Result<AutowrapContext> {
    // HACK: Workaround because finding a resource dir takes a program
    // path rather than a directory path, but then gets the parent path
    let program = args.recipe_path.join("program");

    let resource_dir = brioche_resources::find_output_resource_dir(&program)?;
    let all_resource_dirs = brioche_resources::find_resource_dirs(&program, true)?;

    let mut link_dependencies = vec![];
    if args.self_dependency {
        link_dependencies.push(args.recipe_path.to_owned());
    }
    link_dependencies.extend(args.link_dependencies.iter().cloned());

    let mut link_dependency_library_paths = vec![];
    for link_dep in &link_dependencies {
        let library_path_env_dir = link_dep
            .join("brioche-env.d")
            .join("env")
            .join("LIBRARY_PATH");
        let library_path_env_dir_entries = match std::fs::read_dir(&library_path_env_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read directory {:?}", library_path_env_dir)
                });
            }
        };
        for entry in library_path_env_dir_entries {
            let entry = entry?;
            eyre::ensure!(
                entry.metadata()?.is_symlink(),
                "expected {:?} to be a symlink",
                entry.path()
            );

            let entry_path = entry
                .path()
                .canonicalize()
                .with_context(|| format!("failed to canonicalize path {:?}", entry.path()))?;
            link_dependency_library_paths.push(entry_path);
        }
    }

    let skip_libraries = args
        .dynamic_linking_args
        .skip_libraries
        .iter()
        .map(|lib| &**lib)
        .collect();

    Ok(AutowrapContext {
        args,
        resource_dir,
        all_resource_dirs,
        link_dependencies,
        link_dependency_library_paths,
        skip_libraries,
    })
}

fn try_autowrap_path(ctx: &AutowrapContext, path: &Path) -> eyre::Result<bool> {
    let Some(kind) = autowrap_kind(path)? else {
        return Ok(false);
    };

    match kind {
        AutowrapKind::DynamicBinary => autowrap_dynamic_binary(ctx, path),
        AutowrapKind::SharedLibrary => autowrap_shared_library(ctx, path),
        AutowrapKind::Script => autowrap_script(ctx, path),
        AutowrapKind::Rewrap => autowrap_rewrap(ctx, path),
    }
}

fn autowrap_kind(path: &Path) -> eyre::Result<Option<AutowrapKind>> {
    let contents = std::fs::read(path)?;

    let pack = brioche_pack::extract_pack(&contents[..]);

    if pack.is_ok() {
        Ok(Some(AutowrapKind::Rewrap))
    } else if contents.starts_with(b"#!") {
        Ok(Some(AutowrapKind::Script))
    } else {
        let program_object = goblin::Object::parse(&contents);

        let Ok(goblin::Object::Elf(program_object)) = program_object else {
            return Ok(None);
        };

        if program_object.interpreter.is_some() {
            Ok(Some(AutowrapKind::DynamicBinary))
        } else if program_object.is_lib {
            Ok(Some(AutowrapKind::SharedLibrary))
        } else {
            Ok(None)
        }
    }
}

enum AutowrapKind {
    DynamicBinary,
    SharedLibrary,
    Script,
    Rewrap,
}

fn autowrap_dynamic_binary(ctx: &AutowrapContext, path: &Path) -> eyre::Result<bool> {
    if !ctx.args.dynamic_binary_args.dynamic_binary_enable {
        return Ok(false);
    }

    let contents = std::fs::read(path)?;
    let program_object = goblin::Object::parse(&contents)?;

    let goblin::Object::Elf(program_object) = program_object else {
        eyre::bail!("tried to wrap non-ELF dynamic binary: {}", path.display());
    };

    let Some(interpreter) = program_object.interpreter else {
        eyre::bail!(
            "tried to wrap dynamic binary without an interpreter: {}",
            path.display()
        );
    };
    let relative_interpreter = interpreter.strip_prefix('/').ok_or_else(|| {
        eyre::eyre!("expected program interpreter to start with '/': {interpreter:?}")
    })?;

    let mut interpreter_path = None;
    for dependency in &ctx.link_dependencies {
        let dependency_path = dependency.join(relative_interpreter);
        if dependency_path.exists() {
            interpreter_path = Some(dependency_path);
            break;
        }
    }

    let interpreter_path = interpreter_path
        .ok_or_else(|| eyre::eyre!("could not find interpreter for dynamic binary: {path:?}"))?;
    let interpreter_resource_path = add_named_blob_from(ctx, &interpreter_path)
        .with_context(|| format!("failed to add resource for interpreter {interpreter_path:?}"))?;
    let program_resource_path = add_named_blob_from(ctx, path)
        .with_context(|| format!("failed to add resource for program {path:?}"))?;

    let needed_libraries: VecDeque<_> = program_object
        .libraries
        .iter()
        .copied()
        .filter(|library| !ctx.skip_libraries.contains(*library))
        .chain(
            ctx.args
                .dynamic_binary_args
                .extra_libraries
                .iter()
                .map(|lib| &**lib),
        )
        .map(|lib| lib.to_string())
        .collect();

    let library_dir_resource_paths = collect_all_library_dirs(ctx, needed_libraries)?;

    let program = <Vec<u8>>::from_path_buf(program_resource_path)
        .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))?;
    let interpreter = <Vec<u8>>::from_path_buf(interpreter_resource_path)
        .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))?;
    let library_dirs = library_dir_resource_paths
        .into_iter()
        .map(|resource_path| {
            <Vec<u8>>::from_path_buf(resource_path)
                .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    let pack = brioche_pack::Pack::LdLinux {
        program,
        interpreter,
        library_dirs,
        runtime_library_dirs: vec![],
    };

    let mut packed_exec = std::fs::File::open(
        &ctx.args
            .dynamic_binary_args
            .dynamic_binary_packed_executable,
    )
    .with_context(|| format!("failed to open packed executable {path:?}"))?;
    let mut output =
        std::fs::File::create(path).with_context(|| format!("failed to create file {path:?}"))?;
    std::io::copy(&mut packed_exec, &mut output)
        .with_context(|| format!("failed to copy packed executable to {path:?}"))?;
    brioche_pack::inject_pack(output, &pack)
        .with_context(|| format!("failed to inject pack into {path:?}"))?;

    Ok(true)
}

fn autowrap_shared_library(ctx: &AutowrapContext, path: &Path) -> eyre::Result<bool> {
    if !ctx.args.shared_library_args.shared_library_enable {
        return Ok(false);
    }

    let contents = std::fs::read(path)?;
    let program_object = goblin::Object::parse(&contents)?;

    let goblin::Object::Elf(program_object) = program_object else {
        eyre::bail!("tried to wrap non-ELF dynamic binary: {}", path.display());
    };

    let needed_libraries: VecDeque<_> = program_object
        .libraries
        .iter()
        .copied()
        .filter(|library| !ctx.skip_libraries.contains(*library))
        .chain(
            ctx.args
                .dynamic_binary_args
                .extra_libraries
                .iter()
                .map(|lib| &**lib),
        )
        .map(|lib| lib.to_string())
        .collect();

    let library_dir_resource_paths = collect_all_library_dirs(ctx, needed_libraries)?;

    let library_dirs = library_dir_resource_paths
        .into_iter()
        .map(|resource_path| {
            <Vec<u8>>::from_path_buf(resource_path)
                .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    let pack = brioche_pack::Pack::Static { library_dirs };

    let file = std::fs::OpenOptions::new().append(true).open(path)?;
    brioche_pack::inject_pack(file, &pack)?;

    Ok(true)
}

fn autowrap_script(ctx: &AutowrapContext, path: &Path) -> eyre::Result<bool> {
    if !ctx.args.script_args.script_enable {
        return Ok(false);
    }

    eyre::bail!("tried to wrap script {path:?}, but script wrapping is not yet implemented");
}

fn autowrap_rewrap(ctx: &AutowrapContext, path: &Path) -> eyre::Result<bool> {
    if !ctx.args.rewrap_args.rewrap_enable {
        return Ok(false);
    }

    eyre::bail!("tried to rewrap {path:?}, but rewrapping is not yet implemented");
}

fn collect_all_library_dirs(
    ctx: &AutowrapContext,
    mut needed_libraries: VecDeque<String>,
) -> eyre::Result<Vec<PathBuf>> {
    let mut library_search_paths = ctx.link_dependency_library_paths.clone();
    let mut resource_library_dirs = vec![];
    let mut found_libraries = HashSet::new();
    let mut found_library_dirs = HashSet::new();

    while let Some(library_name) = needed_libraries.pop_front() {
        // If we've already found this library, then skip it
        if found_libraries.contains(&library_name) {
            continue;
        }

        // Find the path to the library
        let library_path = find_library(&library_search_paths, &library_name)?;
        let Some(library_path) = library_path else {
            if ctx.args.dynamic_linking_args.skip_unknown_libraries {
                continue;
            } else {
                eyre::bail!("library not found: {library_name:?}");
            }
        };

        found_libraries.insert(library_name.clone());

        // Don't add the library if it's been skipped. We still do everything
        // else so we can add transitive dependencies even if a library has
        // been skipped
        if !ctx.skip_libraries.contains(&*library_name) {
            // Add the library to the resource directory
            let library_resource_path = add_named_blob_from(ctx, &library_path)
                .with_context(|| format!("failed to add resource for library {library_path:?}"))?;

            // Add the parent dir to the list of library directories. Note
            // that this directory is guaranteed to only contain just this
            // library
            let library_resource_dir = library_resource_path
                .parent()
                .ok_or_eyre("failed to get resource parent dir")?
                .to_owned();

            let is_new_library_path = found_library_dirs.insert(library_resource_dir.clone());
            if is_new_library_path {
                resource_library_dirs.push(library_resource_dir.clone());
            }
        }

        // Try to get the dynamic dependencies from the library itself
        let Ok(library_file) = std::fs::read(&library_path) else {
            continue;
        };
        let Ok(library_object) = goblin::Object::parse(&library_file) else {
            continue;
        };

        // TODO: Support other object files
        let library_elf = match library_object {
            goblin::Object::Elf(elf) => elf,
            _ => {
                continue;
            }
        };
        needed_libraries.extend(library_elf.libraries.iter().map(|lib| lib.to_string()));

        // If the library has a Brioche pack, then use the included resources
        // for additional search directories
        if let Ok(library_pack) = brioche_pack::extract_pack(&library_file[..]) {
            let library_dirs = match &library_pack {
                brioche_pack::Pack::LdLinux { library_dirs, .. } => &library_dirs[..],
                brioche_pack::Pack::Static { library_dirs } => &library_dirs[..],
                brioche_pack::Pack::Metadata { .. } => &[],
            };

            for library_dir in library_dirs {
                let Ok(library_dir) = library_dir.to_path() else {
                    continue;
                };
                let Some(library_dir_path) =
                    brioche_resources::find_in_resource_dirs(&ctx.all_resource_dirs, library_dir)
                else {
                    continue;
                };

                library_search_paths.push(library_dir_path);
            }
        }
    }

    Ok(resource_library_dirs)
}

fn find_library(
    library_search_paths: &[PathBuf],
    library_name: &str,
) -> eyre::Result<Option<PathBuf>> {
    for path in library_search_paths {
        let lib_path = path.join(library_name);
        if lib_path.is_file() {
            return Ok(Some(lib_path));
        }
    }

    Ok(None)
}

fn add_named_blob_from(ctx: &AutowrapContext, path: &Path) -> eyre::Result<PathBuf> {
    use std::os::unix::prelude::PermissionsExt as _;

    let filename = path
        .file_name()
        .ok_or_eyre("failed to get filename from path")?;

    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;

    let permissions = metadata.permissions();
    let mode = permissions.mode();
    let is_executable = mode & 0o111 != 0;

    let mut contents = vec![];
    file.read_to_end(&mut contents)?;

    let resource_path = brioche_resources::add_named_blob(
        &ctx.resource_dir,
        std::io::Cursor::new(contents),
        is_executable,
        Path::new(filename),
    )?;
    Ok(resource_path)
}
