use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::{BufRead as _, Read as _, Write as _},
    path::{Path, PathBuf},
};

use bstr::{ByteSlice as _, ByteVec as _};
use eyre::{Context as _, ContextCompat as _, OptionExt as _};

pub fn pack_source(
    source_path: &Path,
    pack: &brioche_pack::Pack,
    all_resource_dirs: &[PathBuf],
) -> eyre::Result<PackSource> {
    let source = match pack {
        brioche_pack::Pack::LdLinux { program, .. } => {
            let program = program
                .to_path()
                .map_err(|_| eyre::eyre!("invalid program path: {}", bstr::BStr::new(&program)))?;
            let program = brioche_resources::find_in_resource_dirs(all_resource_dirs, program)
                .ok_or_else(|| eyre::eyre!("resource not found: {}", program.display()))?;

            PackSource::Path(program)
        }
        brioche_pack::Pack::Static { .. } => PackSource::This,
        brioche_pack::Pack::Metadata {
            format,
            metadata,
            resource_paths: _,
        } => {
            if format == runnable_core::FORMAT {
                let metadata: runnable_core::Runnable = serde_json::from_slice(metadata)
                    .with_context(|| {
                        format!("failed to deserialize runnable metadata: {metadata:?}")
                    })?;
                let Some(runnable_source) = metadata.source else {
                    eyre::bail!("no source path in metadata");
                };

                let runnable_source_path = match runnable_source.path {
                    runnable_core::RunnablePath::RelativePath { path } => {
                        let path = path
                            .to_path()
                            .map_err(|_| eyre::eyre!("invalid relative path: {path:?}"))?;
                        let new_source_path = source_path.join(path);

                        eyre::ensure!(
                            new_source_path.starts_with(source_path),
                            "relative path {} escapes source path",
                            path.display()
                        );

                        new_source_path
                    }
                    runnable_core::RunnablePath::Resource { resource } => {
                        let resource = resource
                            .to_path()
                            .map_err(|_| eyre::eyre!("invalid resource path: {resource:?}"))?;
                        brioche_resources::find_in_resource_dirs(all_resource_dirs, resource)
                            .ok_or_else(|| eyre::eyre!("resource not found: {resource:?}"))?
                    }
                };

                PackSource::Path(runnable_source_path)
            } else {
                eyre::bail!("unknown metadata format: {format:?}");
            }
        }
    };

    Ok(source)
}

#[derive(Debug)]
pub enum PackSource {
    This,
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct AutopackConfig {
    pub resource_dir: PathBuf,
    pub all_resource_dirs: Vec<PathBuf>,
    pub inputs: AutopackInputs,
    pub quiet: bool,
    pub link_dependencies: Vec<PathBuf>,
    pub dynamic_binary: Option<DynamicBinaryConfig>,
    pub shared_library: Option<SharedLibraryConfig>,
    pub script: Option<ScriptConfig>,
    pub repack: Option<RepackConfig>,
}

#[derive(Debug, Clone)]
pub enum AutopackInputs {
    Paths(Vec<PathBuf>),
    Globs {
        base_path: PathBuf,
        patterns: Vec<String>,
        exclude_patterns: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct DynamicLinkingConfig {
    pub library_paths: Vec<PathBuf>,
    pub skip_libraries: HashSet<String>,
    pub extra_libraries: Vec<String>,
    pub skip_unknown_libraries: bool,
}

#[derive(Debug, Clone)]
pub struct DynamicBinaryConfig {
    pub packed_executable: PathBuf,
    pub extra_runtime_library_paths: Vec<PathBuf>,
    pub dynamic_linking: DynamicLinkingConfig,
}

#[derive(Debug, Clone)]
pub struct SharedLibraryConfig {
    pub dynamic_linking: DynamicLinkingConfig,
    pub allow_empty: bool,
}

#[derive(Debug, Clone)]
pub struct ScriptConfig {
    pub packed_executable: PathBuf,
    pub base_path: Option<PathBuf>,
    pub env: HashMap<String, runnable_core::EnvValue>,
    pub clear_env: bool,
}

impl ScriptConfig {
    /// Returns an iterator of environment variables for autopacked scripts.
    /// Relative paths in the env vars will be adjusted for `output_path`,
    /// so that the paths stay relative to `base_path`.
    ///
    /// For example, if `base_path` is `/output` and `output_path` is
    /// `/output/bin/hello`, then relative paths will be prepended with
    /// a `../` so that they stay relative to `/output`.
    pub fn env_for_output_path<'a>(
        &'a self,
        output_path: &'a Path,
    ) -> impl Iterator<Item = eyre::Result<(String, runnable_core::EnvValue)>> + 'a {
        self.env.iter().map(|(key, env_value)| {
            let env_value = match env_value {
                runnable_core::EnvValue::Clear | runnable_core::EnvValue::Inherit => {
                    env_value.clone()
                }
                runnable_core::EnvValue::Set { value } => {
                    let value = relative_template(value, self.base_path.as_deref(), output_path)?;
                    runnable_core::EnvValue::Set { value }
                }
                runnable_core::EnvValue::Fallback { value } => {
                    let value = relative_template(value, self.base_path.as_deref(), output_path)?;
                    runnable_core::EnvValue::Fallback { value }
                }
                runnable_core::EnvValue::Prepend { value, separator } => {
                    let value = relative_template(value, self.base_path.as_deref(), output_path)?;
                    runnable_core::EnvValue::Prepend {
                        value,
                        separator: separator.clone(),
                    }
                }
                runnable_core::EnvValue::Append { value, separator } => {
                    let value = relative_template(value, self.base_path.as_deref(), output_path)?;
                    runnable_core::EnvValue::Append {
                        value,
                        separator: separator.clone(),
                    }
                }
            };
            eyre::Ok((key.clone(), env_value))
        })
    }
}

fn relative_template(
    value: &runnable_core::Template,
    base_path: Option<&Path>,
    output_path: &Path,
) -> eyre::Result<runnable_core::Template> {
    let Some(base_path) = base_path else {
        return Ok(value.clone());
    };
    let output_path = base_path.join(output_path);
    let output_dir = output_path
        .parent()
        .ok_or_else(|| eyre::eyre!("failed to get parent of output path"))?;

    let components = value
        .components
        .iter()
        .map(|component| -> eyre::Result<_> {
            match component {
                runnable_core::TemplateComponent::Literal { .. }
                | runnable_core::TemplateComponent::Resource { .. } => eyre::Ok(component.clone()),
                runnable_core::TemplateComponent::RelativePath { path } => {
                    // TODO: Handle path resolution in a cross-platform way.
                    // This could change based on the host platform

                    let path = path
                        .to_path()
                        .with_context(|| format!("failed to parse path {path:?}"))?;

                    let full_path = base_path.join(path);
                    let new_relative_path = pathdiff::diff_paths(full_path, output_dir)
                        .context("failed to get path relative to output dir")?;
                    let new_relative_path = <Vec<u8>>::from_path_buf(new_relative_path).map_err(
                        |new_relative_path| {
                            eyre::eyre!("failed to convert path {new_relative_path:?}")
                        },
                    )?;

                    eyre::Ok(runnable_core::TemplateComponent::RelativePath {
                        path: new_relative_path,
                    })
                }
            }
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    Ok(runnable_core::Template { components })
}

#[derive(Debug, Clone)]
pub struct RepackConfig {}

struct AutopackPathConfig {
    can_skip: bool,
}

pub fn autopack(config: &AutopackConfig) -> eyre::Result<()> {
    let ctx = autopack_context(config)?;
    let mut pending_paths = BTreeMap::<PathBuf, AutopackPathConfig>::new();

    match &config.inputs {
        AutopackInputs::Paths(paths) => {
            pending_paths.extend(
                paths
                    .iter()
                    .map(|path| (path.clone(), AutopackPathConfig { can_skip: true })),
            );
        }
        AutopackInputs::Globs {
            base_path,
            patterns,
            exclude_patterns,
        } => {
            let mut globs = globset::GlobSetBuilder::new();
            for pattern in patterns {
                globs.add(globset::Glob::new(pattern)?);
            }

            let mut exclude_globs = globset::GlobSetBuilder::new();
            for pattern in exclude_patterns {
                exclude_globs.add(globset::Glob::new(pattern)?);
            }

            let globs = globs.build()?;
            let exclude_globs = exclude_globs.build()?;

            let walkdir = walkdir::WalkDir::new(base_path);
            for entry in walkdir {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }

                let relative_entry_path = pathdiff::diff_paths(entry.path(), base_path)
                    .ok_or_else(|| {
                        eyre::eyre!(
                            "failed to resolve matched path {} relative to base path {}",
                            entry.path().display(),
                            base_path.display()
                        )
                    })?;

                if globs.is_match(&relative_entry_path)
                    && !exclude_globs.is_match(&relative_entry_path)
                {
                    pending_paths.insert(
                        entry.path().to_owned(),
                        AutopackPathConfig { can_skip: false },
                    );
                }
            }
        }
    }

    while let Some((path, path_config)) = pending_paths.pop_first() {
        autopack_path(&ctx, &path, &path_config, &mut pending_paths)?;
    }

    Ok(())
}

struct AutopackContext<'a> {
    config: &'a AutopackConfig,
    link_dependency_library_paths: Vec<PathBuf>,
    link_dependency_paths: Vec<PathBuf>,
}

fn autopack_context(config: &AutopackConfig) -> eyre::Result<AutopackContext> {
    let mut link_dependency_library_paths = vec![];
    let mut link_dependency_paths = vec![];
    for link_dep in &config.link_dependencies {
        // Add $LIBRARY_PATH directories from symlinks under
        // brioche-env.d/env/LIBRARY_PATH
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
                    format!(
                        "failed to read directory {}",
                        library_path_env_dir.display()
                    )
                });
            }
        };
        for entry in library_path_env_dir_entries {
            let entry = entry?;
            eyre::ensure!(
                entry.file_type()?.is_symlink(),
                "expected {:?} to be a symlink",
                entry.path()
            );

            let entry_path = entry.path().canonicalize().with_context(|| {
                format!("failed to canonicalize path {}", entry.path().display())
            })?;
            link_dependency_library_paths.push(entry_path);
        }
    }

    for link_dep in &config.link_dependencies {
        // Add $PATH directories from symlinks under brioche-env.d/env/PATH
        let path_env_dir = link_dep.join("brioche-env.d").join("env").join("PATH");
        let path_env_dir_entries = match std::fs::read_dir(&path_env_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read directory {}", path_env_dir.display())
                });
            }
        };
        for entry in path_env_dir_entries {
            let entry = entry?;
            eyre::ensure!(
                entry.file_type()?.is_symlink(),
                "expected {:?} to be a symlink",
                entry.path()
            );

            let entry_path = entry.path().canonicalize().with_context(|| {
                format!("failed to canonicalize path {}", entry.path().display())
            })?;
            link_dependency_paths.push(entry_path);
        }
    }

    for link_dep in &config.link_dependencies {
        // Add bin/ to $PATH if it exists
        let link_dep_bin = link_dep.join("bin");
        if link_dep_bin.is_dir() {
            link_dependency_paths.push(link_dep_bin);
        }
    }

    Ok(AutopackContext {
        config,
        link_dependency_library_paths,
        link_dependency_paths,
    })
}

fn autopack_path(
    ctx: &AutopackContext,
    path: &Path,
    path_config: &AutopackPathConfig,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<()> {
    let did_pack = try_autopack_path(ctx, path, path, pending_paths)?;
    if did_pack {
        if !ctx.config.quiet {
            println!("autopacked {}", path.display());
        }
    } else if !path_config.can_skip {
        if !ctx.config.quiet {
            println!("skipped {}", path.display());
        }
    } else {
        eyre::bail!("failed to autopack path: {path:?}");
    }

    Ok(())
}

fn try_autopack_path(
    ctx: &AutopackContext,
    source_path: &Path,
    output_path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<bool> {
    let Some(kind) = autopack_kind(source_path)? else {
        return Ok(false);
    };

    match kind {
        AutopackKind::DynamicBinary => {
            autopack_dynamic_binary(ctx, source_path, output_path, pending_paths)
        }
        AutopackKind::SharedLibrary => {
            autopack_shared_library(ctx, source_path, output_path, pending_paths)
        }
        AutopackKind::Script => autopack_script(ctx, source_path, output_path, pending_paths),
        AutopackKind::Repack => autopack_repack(ctx, source_path, output_path, pending_paths),
    }
}

fn autopack_kind(path: &Path) -> eyre::Result<Option<AutopackKind>> {
    let contents = std::fs::read(path)?;

    let contents_cursor = std::io::Cursor::new(&contents[..]);
    let pack = brioche_pack::extract_pack(contents_cursor);

    if pack.is_ok() {
        Ok(Some(AutopackKind::Repack))
    } else if contents.starts_with(b"#!") {
        Ok(Some(AutopackKind::Script))
    } else {
        let program_object = goblin::Object::parse(&contents);

        let Ok(goblin::Object::Elf(program_object)) = program_object else {
            return Ok(None);
        };

        if program_object.interpreter.is_some() {
            Ok(Some(AutopackKind::DynamicBinary))
        } else if program_object.is_lib {
            Ok(Some(AutopackKind::SharedLibrary))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AutopackKind {
    DynamicBinary,
    SharedLibrary,
    Script,
    Repack,
}

fn autopack_dynamic_binary(
    ctx: &AutopackContext,
    source_path: &Path,
    output_path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<bool> {
    let Some(dynamic_binary_config) = &ctx.config.dynamic_binary else {
        return Ok(false);
    };

    let output_path_parent = output_path
        .parent()
        .ok_or_eyre("could not get parent of output path")?;

    let contents = std::fs::read(source_path)?;
    let program_object = goblin::Object::parse(&contents)?;

    let goblin::Object::Elf(program_object) = program_object else {
        eyre::bail!(
            "tried to autopack non-ELF dynamic binary: {}",
            source_path.display()
        );
    };

    let Some(interpreter) = program_object.interpreter else {
        eyre::bail!(
            "tried to autopack dynamic binary without an interpreter: {}",
            source_path.display()
        );
    };
    let relative_interpreter = interpreter.strip_prefix('/').ok_or_else(|| {
        eyre::eyre!("expected program interpreter to start with '/': {interpreter:?}")
    })?;

    let mut interpreter_path = None;
    for dependency in &ctx.config.link_dependencies {
        let dependency_path = dependency.join(relative_interpreter);
        if dependency_path.exists() {
            interpreter_path = Some(dependency_path);
            break;
        }
    }

    let interpreter_path = interpreter_path.ok_or_else(|| {
        eyre::eyre!("could not find interpreter for dynamic binary: {source_path:?}")
    })?;

    // Autopack the interpreter if it's pending
    try_autopack_dependency(ctx, &interpreter_path, pending_paths)?;

    let interpreter_resource_path = add_named_blob_from(ctx, &interpreter_path, None)
        .with_context(|| {
            format!(
                "failed to add resource for interpreter {}",
                interpreter_path.display()
            )
        })?;
    let program_resource_path = add_named_blob_from(ctx, source_path, None).with_context(|| {
        format!(
            "failed to add resource for program {}",
            source_path.display()
        )
    })?;

    let needed_libraries: VecDeque<_> = program_object
        .libraries
        .iter()
        .copied()
        .chain(
            dynamic_binary_config
                .dynamic_linking
                .extra_libraries
                .iter()
                .map(|lib| &**lib),
        )
        .map(std::string::ToString::to_string)
        .collect();

    let library_dir_resource_paths = collect_all_library_dirs(
        ctx,
        &dynamic_binary_config.dynamic_linking,
        needed_libraries,
        pending_paths,
    )?;

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
    let runtime_library_dirs = dynamic_binary_config
        .extra_runtime_library_paths
        .iter()
        .map(|path| {
            let path = pathdiff::diff_paths(path, output_path_parent).ok_or_else(|| eyre::eyre!("failed to get relative path from output path {output_path_parent:?} to runtime library path {path:?}"))?;
            <Vec<u8>>::from_path_buf(path)
                .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let pack = brioche_pack::Pack::LdLinux {
        program,
        interpreter,
        library_dirs,
        runtime_library_dirs,
    };

    let packed_exec_path = &dynamic_binary_config.packed_executable;
    let mut packed_exec = std::fs::File::open(packed_exec_path).with_context(|| {
        format!(
            "failed to open packed executable {}",
            packed_exec_path.display()
        )
    })?;
    let mut output = std::fs::File::create(output_path)
        .with_context(|| format!("failed to create file {}", output_path.display()))?;
    std::io::copy(&mut packed_exec, &mut output).with_context(|| {
        format!(
            "failed to copy packed executable to {}",
            output_path.display()
        )
    })?;
    brioche_pack::inject_pack(output, &pack)
        .with_context(|| format!("failed to inject pack into {}", output_path.display()))?;

    Ok(true)
}

fn autopack_shared_library(
    ctx: &AutopackContext,
    source_path: &Path,
    output_path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<bool> {
    let Some(shared_library_config) = &ctx.config.shared_library else {
        return Ok(false);
    };

    let contents = std::fs::read(source_path)?;
    let program_object = goblin::Object::parse(&contents)?;

    let goblin::Object::Elf(program_object) = program_object else {
        eyre::bail!(
            "tried to autopack non-ELF dynamic binary: {}",
            source_path.display()
        );
    };

    let needed_libraries: VecDeque<_> = program_object
        .libraries
        .iter()
        .copied()
        .filter(|library| {
            !shared_library_config
                .dynamic_linking
                .skip_libraries
                .contains(*library)
        })
        .chain(
            shared_library_config
                .dynamic_linking
                .extra_libraries
                .iter()
                .map(|lib| &**lib),
        )
        .map(std::string::ToString::to_string)
        .collect();

    let library_dir_resource_paths = collect_all_library_dirs(
        ctx,
        &shared_library_config.dynamic_linking,
        needed_libraries,
        pending_paths,
    )?;

    let library_dirs = library_dir_resource_paths
        .into_iter()
        .map(|resource_path| {
            <Vec<u8>>::from_path_buf(resource_path)
                .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))
        })
        .collect::<eyre::Result<Vec<_>>>()?;
    let pack = brioche_pack::Pack::Static { library_dirs };

    if !pack.should_add_to_executable() && !shared_library_config.allow_empty {
        return Ok(false);
    }

    let file = if source_path == output_path {
        std::fs::OpenOptions::new().append(true).open(output_path)?
    } else {
        let mut new_file = std::fs::File::create(output_path)?;
        new_file.write_all(&contents)?;
        new_file
    };
    brioche_pack::inject_pack(file, &pack)?;

    Ok(true)
}

fn autopack_script(
    ctx: &AutopackContext,
    source_path: &Path,
    output_path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<bool> {
    let Some(script_config) = &ctx.config.script else {
        return Ok(false);
    };

    let script_file = std::fs::File::open(source_path)?;
    let mut script_file = std::io::BufReader::new(script_file);
    let mut shebang = [0; 2];
    let Ok(()) = script_file.read_exact(&mut shebang) else {
        return Ok(false);
    };
    if shebang != *b"#!" {
        return Ok(false);
    }

    let mut shebang_line = String::new();
    script_file.read_line(&mut shebang_line)?;

    let shebang_line = shebang_line.trim();
    let shebang_parts = shebang_line.split_once(|c: char| c.is_ascii_whitespace());
    let (command_path, arg) = match shebang_parts {
        Some((command_path, arg)) => (command_path.trim(), arg.trim()),
        None => (shebang_line, ""),
    };

    let mut arg = Some(arg).filter(|arg| !arg.is_empty());
    let mut command_name = command_path
        .split(['/', '\\'])
        .next_back()
        .unwrap_or(command_path);

    if command_name == "env" {
        command_name = arg.ok_or_eyre("expected argument for env script")?;
        arg = None;
    }
    let mut command = None;
    for link_dependency_path in &ctx.link_dependency_paths {
        if link_dependency_path.join(command_name).is_file() {
            command = Some(link_dependency_path.join(command_name));
            break;
        }
    }

    let command = command.ok_or_else(|| eyre::eyre!("could not find command {command_name:?}"))?;

    // Autopack the command if it's pending
    try_autopack_dependency(ctx, &command, pending_paths)?;

    let command_resource = add_named_blob_from(ctx, &command, None)?;
    let script_resource = add_named_blob_from(ctx, source_path, None)?;

    let env_resource_paths = script_config
        .env
        .values()
        .filter_map(|value| match value {
            runnable_core::EnvValue::Clear | runnable_core::EnvValue::Inherit => None,
            runnable_core::EnvValue::Set { value }
            | runnable_core::EnvValue::Fallback { value }
            | runnable_core::EnvValue::Prepend {
                value,
                separator: _,
            }
            | runnable_core::EnvValue::Append {
                value,
                separator: _,
            } => Some(value),
        })
        .flat_map(|template| &template.components)
        .filter_map(|component| match component {
            runnable_core::TemplateComponent::Literal { .. }
            | runnable_core::TemplateComponent::RelativePath { .. } => None,
            runnable_core::TemplateComponent::Resource { resource } => Some(
                resource
                    .to_path()
                    .map_err(|_| eyre::eyre!("invalid resource path")),
            ),
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let resource_paths = [command_resource.clone(), script_resource.clone()]
        .into_iter()
        .chain(
            env_resource_paths
                .into_iter()
                .map(std::borrow::ToOwned::to_owned),
        )
        .map(|path| {
            Vec::<u8>::from_path_buf(path).map_err(|_| eyre::eyre!("invalid resource path"))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    let command = runnable_core::Template::from_resource_path(command_resource)?;

    let mut args = vec![];
    if let Some(arg) = arg {
        args.push(runnable_core::ArgValue::Arg {
            value: runnable_core::Template::from_literal(arg.into()),
        });
    }
    args.push(runnable_core::ArgValue::Arg {
        value: runnable_core::Template::from_resource_path(script_resource.clone())?,
    });
    args.push(runnable_core::ArgValue::Rest);

    let env = script_config
        .env_for_output_path(output_path)
        .collect::<eyre::Result<_>>()?;

    let runnable_pack = runnable_core::Runnable {
        command,
        args,
        env,
        clear_env: script_config.clear_env,
        source: Some(runnable_core::RunnableSource {
            path: runnable_core::RunnablePath::from_resource_path(script_resource)?,
        }),
    };
    let pack = brioche_pack::Pack::Metadata {
        resource_paths,
        format: runnable_core::FORMAT.to_string(),
        metadata: serde_json::to_vec(&runnable_pack)?,
    };

    let packed_exec_path = &script_config.packed_executable;
    let mut packed_exec = std::fs::File::open(packed_exec_path).with_context(|| {
        format!(
            "failed to open packed executable {}",
            packed_exec_path.display()
        )
    })?;

    let mut output = std::fs::File::create(output_path)
        .with_context(|| format!("failed to create file {}", output_path.display()))?;
    std::io::copy(&mut packed_exec, &mut output).with_context(|| {
        format!(
            "failed to copy packed executable to {}",
            output_path.display()
        )
    })?;
    brioche_pack::inject_pack(output, &pack)
        .with_context(|| format!("failed to inject pack into {}", output_path.display()))?;

    Ok(true)
}

fn autopack_repack(
    ctx: &AutopackContext,
    source_path: &Path,
    output_path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<bool> {
    let Some(_) = &ctx.config.repack else {
        return Ok(false);
    };

    let contents = std::fs::read(source_path)?;
    let extracted = brioche_pack::extract_pack(std::io::Cursor::new(&contents))?;

    let repack_source = pack_source(source_path, &extracted.pack, &ctx.config.all_resource_dirs)
        .with_context(|| format!("failed to repack {}", source_path.display()))?;

    let unpacked_source_path;
    let unpacked_output_path;
    match repack_source {
        PackSource::This => {
            // Write the unpacked contents to the output path
            let unpacked_contents = &contents[..extracted.unpacked_len];
            std::fs::write(output_path, unpacked_contents).with_context(|| {
                format!(
                    "failed to write unpacked contents to {}",
                    output_path.display()
                )
            })?;

            // Repack the unpacked contents directly at the output path
            unpacked_source_path = output_path.to_owned();
            unpacked_output_path = output_path.to_owned();
        }
        PackSource::Path(path) => {
            // Repack the source path and write to the output path
            unpacked_source_path = path;
            unpacked_output_path = output_path.to_owned();
        }
    }

    let result = try_autopack_path(
        ctx,
        &unpacked_source_path,
        &unpacked_output_path,
        pending_paths,
    )?;
    Ok(result)
}

fn collect_all_library_dirs(
    ctx: &AutopackContext,
    dynamic_linking_config: &DynamicLinkingConfig,
    mut needed_libraries: VecDeque<String>,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<Vec<PathBuf>> {
    let mut library_search_paths = vec![];
    let mut resource_library_dirs = vec![];
    let mut found_libraries = HashSet::new();
    let mut found_library_dirs = HashSet::new();

    library_search_paths.extend_from_slice(&dynamic_linking_config.library_paths);
    library_search_paths.extend_from_slice(&ctx.link_dependency_library_paths);

    while let Some(library_name) = needed_libraries.pop_front() {
        // If we've already found this library, then skip it
        if found_libraries.contains(&library_name) {
            continue;
        }

        // Find the path to the library
        let library_path = find_library(&library_search_paths, &library_name)?;
        let Some(library_path) = library_path else {
            if dynamic_linking_config.skip_unknown_libraries {
                continue;
            }

            eyre::bail!("library not found: {library_name:?}");
        };

        // Autopack the library if it's pending
        try_autopack_dependency(ctx, &library_path, pending_paths)?;

        found_libraries.insert(library_name.clone());

        // Don't add the library if it's been skipped. We still do everything
        // else so we can add transitive dependencies even if a library has
        // been skipped
        if !dynamic_linking_config
            .skip_libraries
            .contains(&*library_name)
        {
            // Add the library to the resource directory
            let library_alias = Path::new(&library_name);
            let library_resource_path =
                add_named_blob_from(ctx, &library_path, Some(library_alias)).with_context(
                    || {
                        format!(
                            "failed to add resource for library {}",
                            library_path.display()
                        )
                    },
                )?;

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
        let goblin::Object::Elf(library_elf) = library_object else {
            continue;
        };
        needed_libraries.extend(library_elf.libraries.iter().map(|lib| (*lib).to_string()));

        // If the library has a Brioche pack, then use the included resources
        // for additional search directories
        let library_file_cursor = std::io::Cursor::new(&library_file[..]);
        if let Ok(extracted_library) = brioche_pack::extract_pack(library_file_cursor) {
            let library_dirs = match &extracted_library.pack {
                brioche_pack::Pack::Static { library_dirs }
                | brioche_pack::Pack::LdLinux { library_dirs, .. } => &library_dirs[..],
                brioche_pack::Pack::Metadata { .. } => &[],
            };

            for library_dir in library_dirs {
                let Ok(library_dir) = library_dir.to_path() else {
                    continue;
                };
                let Some(library_dir_path) = brioche_resources::find_in_resource_dirs(
                    &ctx.config.all_resource_dirs,
                    library_dir,
                ) else {
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
    let mut library_search_path_files = vec![];

    // Try to find a direct filename match from the search paths
    for path in library_search_paths {
        if path.is_dir() {
            // Check if the search path is a directory and contains a file
            // matching the library name
            let lib_path = path.join(library_name);
            if lib_path.is_file() {
                return Ok(Some(lib_path));
            }
        } else if path.is_file() {
            // Check if the search path is a file that matches the library
            // name directly
            let path_filename = path
                .file_name()
                .ok_or_eyre("failed to get filename from path")?;
            if path_filename.to_str() == Some(library_name) {
                return Ok(Some(path.to_owned()));
            }

            // If the filename doesn't match, queue it for a further check
            // if we don't find another path-based match
            library_search_path_files.push(path);
        }
    }

    // Try to find a library file that matches based on its `DT_SONAME` field
    // as a fallback
    for &path in &library_search_path_files {
        let Ok(contents) = std::fs::read(path) else {
            continue;
        };

        let Ok(elf) = goblin::elf::Elf::parse(&contents) else {
            continue;
        };

        if elf.soname == Some(library_name) {
            return Ok(Some(path.to_owned()));
        }
    }

    Ok(None)
}

fn add_named_blob_from(
    ctx: &AutopackContext,
    path: &Path,
    alias_name: Option<&Path>,
) -> eyre::Result<PathBuf> {
    use std::os::unix::prelude::PermissionsExt as _;

    let alias_name = if let Some(alias_name) = alias_name {
        alias_name
    } else {
        let filename = path
            .file_name()
            .ok_or_eyre("failed to get filename from path")?;
        Path::new(filename)
    };

    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;

    let permissions = metadata.permissions();
    let mode = permissions.mode();
    let is_executable = mode & 0o111 != 0;

    let file_reader = std::io::BufReader::new(file);
    let resource_path = brioche_resources::add_named_blob(
        &ctx.config.resource_dir,
        file_reader,
        is_executable,
        alias_name,
    )?;
    Ok(resource_path)
}

fn try_autopack_dependency(
    ctx: &AutopackContext,
    path: &Path,
    pending_paths: &mut BTreeMap<PathBuf, AutopackPathConfig>,
) -> eyre::Result<()> {
    // Get the canonical path of the dependency
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path {}", path.display()))?;

    // If the path is pending, then autopack it
    if let Some(path_config) = pending_paths.remove(&canonical_path) {
        autopack_path(ctx, path, &path_config, pending_paths)?;
    }

    Ok(())
}
