use std::{ffi::OsString, os::unix::process::CommandExt as _, path::PathBuf, process::ExitCode};

use bstr::ByteSlice as _;

const BRIOCHE_PACKED_ERROR: u8 = 121;

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

                if runnable.clear_env {
                    command.env_clear();
                }

                for (env_name, env_value) in &runnable.env {
                    match env_value {
                        runnable_core::EnvValue::Clear => {
                            command.env_remove(env_name);
                        }
                        runnable_core::EnvValue::Inherit => {
                            let value = std::env::var_os(env_name);
                            if let Some(value) = value {
                                command.env(env_name, value);
                            }
                        }
                        runnable_core::EnvValue::Set { value } => {
                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            command.env(env_name, value);
                        }
                        runnable_core::EnvValue::Fallback { value } => {
                            let current_value = std::env::var_os(env_name);
                            let current_value = current_value.filter(|value| !value.is_empty());
                            let value = match current_value {
                                Some(current_value) => current_value,
                                None => value.to_os_string(&program_path, &resource_dirs)?,
                            };
                            command.env(env_name, value);
                        }
                        runnable_core::EnvValue::Prepend { value, separator } => {
                            let mut value = value.to_os_string(&program_path, &resource_dirs)?;
                            let separator =
                                separator
                                    .to_os_str()
                                    .map_err(|_| PackedError::InvalidUtf8 {
                                        bytes: separator.clone().into(),
                                    })?;

                            let current_value = std::env::var_os(env_name);
                            let new_value = match current_value {
                                Some(current_value) if !current_value.is_empty() => {
                                    value.push(separator);
                                    value.push(current_value);

                                    value
                                }
                                _ => value,
                            };
                            command.env(env_name, new_value);
                        }
                        runnable_core::EnvValue::Append { value, separator } => {
                            let value = value.to_os_string(&program_path, &resource_dirs)?;
                            let separator =
                                separator
                                    .to_os_str()
                                    .map_err(|_| PackedError::InvalidUtf8 {
                                        bytes: separator.clone().into(),
                                    })?;

                            let current_value = std::env::var_os(env_name);
                            let new_value = match current_value {
                                Some(mut current_value) if !current_value.is_empty() => {
                                    current_value.push(separator);
                                    current_value.push(value);

                                    current_value
                                }
                                _ => value,
                            };
                            command.env(env_name, new_value);
                        }
                    }
                }

                let error = command.exec();
                Err(PackedError::IoError(error))
            }
            _ => {
                unimplemented!("unknown metdata format {format:?}");
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
    #[error("unconvertable path: {path:?}")]
    InvalidPathOsString { path: OsString },
}
