use std::{
    collections::HashMap, ffi::OsString, os::unix::process::CommandExt as _, path::PathBuf,
    process::ExitCode,
};

use bstr::{ByteSlice as _, ByteVec as _};

const BRIOCHE_PACKED_ERROR: u8 = 121;
const PATH_SEPARATOR: &str = ":";

#[must_use]
pub fn main() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("brioche-packed error: {err}");
            ExitCode::from(BRIOCHE_PACKED_ERROR)
        }
    }
}

fn run() -> Result<(), PackedError> {
    let program_path = std::env::current_exe()?;
    let program_parent_path = program_path
        .parent()
        .ok_or_else(|| PackedError::InvalidPath {
            path: program_path.clone(),
        })?;
    let resource_dirs = brioche_resources::find_resource_dirs(&program_path, true)?;
    let mut program = std::fs::File::open(&program_path)?;
    let extracted = brioche_pack::extract_pack(&mut program)?;

    match extracted.pack {
        brioche_pack::Pack::LdLinux {
            program,
            interpreter,
            library_dirs,
            runtime_library_dirs,
        } => {
            let mut args = std::env::args_os();

            let interpreter = interpreter
                .to_path()
                .map_err(|_| PackedError::InvalidPathBytes {
                    path: interpreter.clone().into(),
                })?;
            let interpreter = brioche_resources::find_in_resource_dirs(&resource_dirs, interpreter)
                .ok_or_else(|| PackedError::ResourceNotFound {
                    resource: interpreter.to_owned(),
                })?;
            let mut command = std::process::Command::new(interpreter);

            let mut resolved_library_dirs = vec![];

            for library_dir in &runtime_library_dirs {
                let library_dir =
                    library_dir
                        .to_path()
                        .map_err(|_| PackedError::InvalidPathBytes {
                            path: library_dir.clone().into(),
                        })?;
                let resolved_library_dir = program_parent_path.join(library_dir);
                resolved_library_dirs.push(resolved_library_dir);
            }

            for library_dir in &library_dirs {
                let library_dir =
                    library_dir
                        .to_path()
                        .map_err(|_| PackedError::InvalidPathBytes {
                            path: library_dir.clone().into(),
                        })?;
                let library_dir =
                    brioche_resources::find_in_resource_dirs(&resource_dirs, library_dir)
                        .ok_or_else(|| PackedError::ResourceNotFound {
                            resource: library_dir.to_owned(),
                        })?;
                resolved_library_dirs.push(library_dir);
            }

            if !resolved_library_dirs.is_empty() {
                let mut ld_library_path = bstr::BString::default();
                for (n, library_dir) in resolved_library_dirs.iter().enumerate() {
                    if n > 0 {
                        ld_library_path.push(b':');
                    }

                    let path =
                        <[u8]>::from_path(library_dir).ok_or_else(|| PackedError::InvalidPath {
                            path: library_dir.to_owned(),
                        })?;
                    ld_library_path.extend(path);
                }

                if let Some(env_library_path) = std::env::var_os("LD_LIBRARY_PATH") {
                    let env_library_path =
                        <[u8]>::from_os_str(&env_library_path).ok_or_else(|| {
                            PackedError::InvalidPathOsString {
                                path: env_library_path.clone(),
                            }
                        })?;
                    if !env_library_path.is_empty() {
                        ld_library_path.push(b':');
                        ld_library_path.extend(env_library_path);
                    }
                }

                command.arg("--library-path");

                let ld_library_path =
                    ld_library_path
                        .to_os_str()
                        .map_err(|_| PackedError::InvalidPathBytes {
                            path: ld_library_path.clone(),
                        })?;
                command.arg(ld_library_path);
            }

            if let Some(arg0) = args.next() {
                command.arg("--argv0");
                command.arg(arg0);
            }

            let program = program
                .to_path()
                .map_err(|_| PackedError::InvalidPathBytes {
                    path: program.clone().into(),
                })?;
            let program = brioche_resources::find_in_resource_dirs(&resource_dirs, program)
                .ok_or_else(|| PackedError::ResourceNotFound {
                    resource: program.to_owned(),
                })?;
            let program = program.canonicalize()?;
            command.arg(program);

            command.args(args);

            let error = command.exec();
            Err(PackedError::IoError(error))
        }
        brioche_pack::Pack::Static { .. } => {
            unimplemented!("execution of a static executable");
        }
        brioche_pack::Pack::Metadata {
            resource_paths: _,
            format,
            metadata,
        } => match &*format {
            runnable_core::FORMAT => {
                let runnable: runnable_core::Runnable = serde_json::from_slice(&metadata)?;

                let program = runnable
                    .command
                    .to_os_string(&program_path, &resource_dirs)?;

                let mut command = std::process::Command::new(program);
                let mut original_args = Some(std::env::args_os().skip(1));

                for arg in &runnable.args {
                    match arg {
                        runnable_core::ArgValue::Arg { value } => {
                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            command.arg(value);
                        }
                        runnable_core::ArgValue::Rest => {
                            let original_args =
                                original_args.take().ok_or(PackedError::RepeatedArgs)?;
                            command.args(original_args);
                        }
                    }
                }

                let mut envs = EnvVarChanges::new(runnable.clear_env);

                // Clear/inherit explicit env vars up front before applying any
                // other env var changes
                for (env_var, env_value) in &runnable.env {
                    match env_value {
                        runnable_core::EnvValue::Set { .. } => {
                            // Set - do nothing, the env var will be overridden
                        }
                        runnable_core::EnvValue::Clear => {
                            // Clear - start with an initial blank value
                            envs.clear(env_var.to_string());
                        }
                        runnable_core::EnvValue::Fallback { value } => {
                            // Fallback - explicitly inherit the env var, then
                            // set an initial value if not already set
                            envs.inherit(env_var.to_string());

                            let inherited_value = envs.get_mut(env_var.to_string());
                            if inherited_value.is_none() {
                                let value = value.to_os_string(&program_path, &resource_dirs)?;
                                *inherited_value = Some(value);
                            }
                        }
                        runnable_core::EnvValue::Inherit
                        | runnable_core::EnvValue::Prepend { .. }
                        | runnable_core::EnvValue::Append { .. } => {
                            // Inherit, prepend, and append should all start
                            // with the inherited env var initially before
                            // making any other changes
                            envs.inherit(env_var.to_string());
                        }
                    }
                }

                // Apply env vars from dependencies
                for dependency in runnable.dependencies {
                    let dependency_path = dependency.to_path(&program_path, &resource_dirs)?;

                    // Try to read the `brioche-env.d/env` directory from the
                    // dependency. Each entry within the directory will set
                    // an env var based on the entry name
                    let env_dir = dependency_path.join("brioche-env.d/env");
                    let env_dir_entries = std::fs::read_dir(&env_dir).into_iter().flatten();
                    for env_dir_entry in env_dir_entries {
                        let env_dir_entry = env_dir_entry?;

                        let env_var = env_dir_entry.file_name().into_string().map_err(|_| {
                            PackedError::InvalidDependencyEnvVar {
                                dependency: dependency_path.clone(),
                                env_var: env_dir_entry.file_name(),
                            }
                        })?;
                        let env_dir_entry_path = env_dir_entry.path();
                        let env_dir_entry_type = env_dir_entry.file_type()?;

                        if env_dir_entry_type.is_dir() {
                            // Directory - each sub-entry should be a symlink.
                            // The symlink targets will be appended to the env
                            // var using the path separator

                            let env_value_entries = std::fs::read_dir(&env_dir_entry_path)?;
                            let mut env_value_entries = env_value_entries
                                .into_iter()
                                .map(|entry| entry.map_err(PackedError::IoError))
                                .collect::<Result<Vec<_>, PackedError>>()?;
                            env_value_entries.sort_by_key(std::fs::DirEntry::file_name);

                            let mut env_value_append = OsString::new();
                            for (i, env_value_entry) in env_value_entries.into_iter().enumerate() {
                                if i != 0 {
                                    env_value_append.push(PATH_SEPARATOR);
                                }

                                let env_value_entry_type = env_value_entry.file_type()?;
                                if !env_value_entry_type.is_symlink() {
                                    return Err(PackedError::InvalidDependencyEnvVar {
                                        dependency: dependency_path,
                                        env_var: env_dir_entry.file_name(),
                                    });
                                }

                                let value_path = std::fs::canonicalize(env_value_entry.path())?;
                                env_value_append.push(value_path);
                            }

                            envs.append(env_var, env_value_append, PATH_SEPARATOR.as_ref());
                        } else if env_dir_entry_type.is_file() {
                            // File - the file's contents will be used as a
                            // fallback value for the env var

                            let current_value = envs.get_mut(env_var);
                            if current_value.is_none() {
                                let content = std::fs::read(env_dir_entry.path())?;
                                let content = content.into_os_string().map_err(|_| {
                                    PackedError::InvalidDependencyEnvVar {
                                        dependency: dependency_path.clone(),
                                        env_var: env_dir_entry.file_name(),
                                    }
                                })?;
                                *current_value = Some(content);
                            }
                        } else if env_dir_entry_type.is_symlink() {
                            // Symlink - the symlink target path will be used
                            // as a fallback value for the env var

                            let current_value = envs.get_mut(env_var);
                            if current_value.is_none() {
                                let value_path = std::fs::canonicalize(env_dir_entry.path())?;
                                *current_value = Some(value_path.into_os_string());
                            }
                        }
                    }
                }

                // Finally, apply the explicitly-set env vars
                for (env_name, env_value) in runnable.env {
                    match &env_value {
                        runnable_core::EnvValue::Clear
                        | runnable_core::EnvValue::Inherit
                        | runnable_core::EnvValue::Fallback { .. } => {
                            // Already applied beforehand
                        }
                        runnable_core::EnvValue::Set { value } => {
                            // Override the env var with the provided value

                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            envs.set(env_name, value);
                        }
                        runnable_core::EnvValue::Prepend { value, separator } => {
                            // Prepend the env var

                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            let separator =
                                separator
                                    .to_os_str()
                                    .map_err(|_| PackedError::InvalidUtf8 {
                                        bytes: separator.clone().into(),
                                    })?;

                            envs.prepend(env_name, value, separator);
                        }
                        runnable_core::EnvValue::Append { value, separator } => {
                            // Append the env var

                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            let separator =
                                separator
                                    .to_os_str()
                                    .map_err(|_| PackedError::InvalidUtf8 {
                                        bytes: separator.clone().into(),
                                    })?;

                            envs.append(env_name, value, separator);
                        }
                    }
                }

                // Apply the accumulated env var changes to the command
                envs.apply_to_command(&mut command);

                let error = command.exec();
                Err(PackedError::IoError(error))
            }
            _ => {
                unimplemented!("unknown metadata format {format:?}");
            }
        },
    }
}

#[derive(Debug, thiserror::Error)]
enum PackedError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    SerdeJsonError(#[from] serde_json::Error),
    #[error(transparent)]
    ExtractPackError(#[from] brioche_pack::ExtractPackError),
    #[error(transparent)]
    PackResourceDirError(#[from] brioche_resources::PackResourceDirError),
    #[error(transparent)]
    RunnableTemplateError(#[from] runnable_core::RunnableTemplateError),
    #[error("tried to pass remaining arguments more than once")]
    RepeatedArgs,
    #[error("resource not found: {resource}")]
    ResourceNotFound { resource: PathBuf },
    #[error("invalid UTF-8: {bytes:?}")]
    InvalidUtf8 { bytes: bstr::BString },
    #[error("invalid path: {path:?}")]
    InvalidPathBytes { path: bstr::BString },
    #[error("invalid path: {path:?}")]
    InvalidPath { path: PathBuf },
    #[error("unconvertible path: {path:?}")]
    InvalidPathOsString { path: OsString },
    #[error("invalid env var {env_var:?} in dependency: {dependency:?}")]
    InvalidDependencyEnvVar {
        dependency: PathBuf,
        env_var: OsString,
    },
}

struct EnvVarChanges {
    clear_envs: bool,
    envs: HashMap<String, Option<OsString>>,
}

impl EnvVarChanges {
    fn new(clear_envs: bool) -> Self {
        Self {
            clear_envs,
            envs: HashMap::new(),
        }
    }

    fn get_mut(&mut self, env_var: String) -> &mut Option<OsString> {
        self.envs.entry(env_var).or_insert_with_key(|env_var| {
            if self.clear_envs {
                None
            } else {
                std::env::var_os(env_var)
            }
        })
    }

    fn inherit(&mut self, env_var: String) {
        let value = std::env::var_os(&env_var);
        self.envs.insert(env_var, value);
    }

    fn set(&mut self, env_var: String, value: OsString) {
        self.envs.insert(env_var, Some(value));
    }

    fn clear(&mut self, env_var: String) {
        self.envs.insert(env_var, None);
    }

    fn prepend(&mut self, env_var: String, mut value: OsString, separator: &std::ffi::OsStr) {
        let env_value = self.get_mut(env_var);

        if let Some(current_value) = env_value.take() {
            value.push(separator);
            value.push(current_value);
        }

        *env_value = Some(value);
    }

    fn append(&mut self, env_var: String, value: OsString, separator: &std::ffi::OsStr) {
        let current_value = self.get_mut(env_var);
        if let Some(current_value) = current_value {
            current_value.push(separator);
            current_value.push(value);
        } else {
            *current_value = Some(value);
        }
    }

    fn apply_to_command(self, command: &mut std::process::Command) {
        if self.clear_envs {
            command.env_clear();
        }

        for (env_var, env_value) in self.envs {
            if let Some(env_value) = env_value {
                command.env(env_var, env_value);
            } else {
                command.env_remove(env_var);
            }
        }
    }
}
