use std::{
    io::{Read as _, Seek},
    path::PathBuf,
    process::ExitCode,
};

use bstr::{ByteSlice as _, ByteVec as _};
use eyre::{Context as _, OptionExt as _};

#[derive(Debug)]
enum StripArg {
    Arg(std::ffi::OsString),
    DashOPath(std::path::PathBuf),
    DashOFollowedByPath(std::path::PathBuf),
    InputPath(std::path::PathBuf),
}

impl StripArg {
    fn into_args(self) -> eyre::Result<Vec<std::ffi::OsString>> {
        match self {
            StripArg::Arg(arg) => Ok(vec![arg]),
            StripArg::DashOPath(path) => {
                let path_bytes = <[u8]>::from_path(&path).ok_or_eyre("invalid path")?;

                let mut arg = b"-o".to_vec();
                arg.extend_from_slice(path_bytes);
                Ok(vec![arg.to_os_str()?.to_owned()])
            }
            StripArg::DashOFollowedByPath(path) => Ok(vec![
                std::ffi::OsString::from("-o"),
                std::ffi::OsString::from(path),
            ]),
            StripArg::InputPath(path) => Ok(vec![std::ffi::OsString::from(path)]),
        }
    }
}

fn main() -> ExitCode {
    let result = run();

    match result {
        Ok(exit_code) => exit_code,
        Err(err) => {
            eprintln!("{:#}", err);
            ExitCode::FAILURE
        }
    }
}

fn run() -> eyre::Result<ExitCode> {
    let current_exe = std::env::current_exe().context("failed to get current executable")?;
    let current_exe_dir = current_exe
        .parent()
        .ok_or_eyre("failed to get current executable dir")?;
    let current_exe_parent_dir = current_exe_dir
        .parent()
        .ok_or_eyre("failed to get current executable dir")?;
    let strip_resource_dir = current_exe_parent_dir.join("libexec").join("brioche-strip");
    if !strip_resource_dir.is_dir() {
        eyre::bail!(
            "failed to find strip resource dir: {}",
            strip_resource_dir.display()
        );
    }

    let strip = strip_resource_dir.join("strip");

    // If autopacking is disabled, call the original `strip` binary and
    // bail early
    if let Ok("false") = std::env::var("BRIOCHE_STRIP_AUTOPACK").as_deref() {
        let mut command = std::process::Command::new(strip);
        command.args(std::env::args_os().skip(1));
        let status = command.status()?;

        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map(ExitCode::from)
            .unwrap_or(ExitCode::FAILURE);
        return Ok(exit_code);
    }

    let mut args = std::env::args_os().skip(1);
    let mut strip_args = vec![];

    while let Some(arg) = args.next() {
        let arg_bytes = <[u8]>::from_os_str(&arg).ok_or_eyre("invalid arg")?;
        let arg_bytes = bstr::BStr::new(arg_bytes);

        match &**arg_bytes {
            b"-F"
            | b"--target"
            | b"-I"
            | b"--input-target"
            | b"-O"
            | b"--output-target"
            | b"-K"
            | b"--keep-symbol"
            | b"-N"
            | b"--strip-symbol"
            | b"-R"
            | b"--remove-section"
            | b"--remove-relocations" => {
                let next_arg = args.next().ok_or_eyre("expected arg after flag")?;
                strip_args.extend([StripArg::Arg(arg), StripArg::Arg(next_arg)]);
            }
            b"-o" => {
                let output = args.next().ok_or_eyre("expected path after -o")?;
                let output = std::path::PathBuf::from(output);
                strip_args.push(StripArg::DashOFollowedByPath(output))
            }
            _ => {
                if let Some(output) = arg_bytes.strip_prefix(b"-o") {
                    let output = output.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
                    strip_args.push(StripArg::DashOPath(output.to_path_buf()))
                } else if arg_bytes.starts_with(b"-") {
                    strip_args.push(StripArg::Arg(arg));
                } else if arg_bytes.starts_with(b"@") {
                    eyre::bail!("using @ for passing args is not supported");
                } else {
                    let input_path = arg_bytes
                        .to_path()
                        .map_err(|_| eyre::eyre!("invalid path"))?;
                    strip_args.push(StripArg::InputPath(input_path.to_owned()));
                }
            }
        }
    }

    let mut remapped_files = vec![];
    remap_files(&mut strip_args, &mut remapped_files)?;

    let strip_args = strip_args
        .into_iter()
        .map(|arg| arg.into_args())
        .collect::<eyre::Result<Vec<_>>>()?;

    let mut command = std::process::Command::new(strip);
    command.args(strip_args.iter().flatten());
    let status = command.status()?;

    if !status.success() {
        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map(ExitCode::from)
            .unwrap_or(ExitCode::FAILURE);
        return Ok(exit_code);
    }

    for remapped_file in remapped_files {
        finish_remapped_file(remapped_file)?;
    }

    Ok(ExitCode::SUCCESS)
}

enum RemapFile {
    Inject {
        pack: brioche_pack::Pack,
        temp_file: tempfile::NamedTempFile,
        output_path: PathBuf,
    },
    UpdateSource {
        extracted: brioche_pack::ExtractedPack,
        input_path: PathBuf,
        temp_file: tempfile::NamedTempFile,
        output_path: PathBuf,
    },
}

fn remap_files(args: &mut Vec<StripArg>, remapped_files: &mut Vec<RemapFile>) -> eyre::Result<()> {
    let output_path_index = args.iter().enumerate().find_map(|(n, arg)| match arg {
        StripArg::DashOPath(_) | StripArg::DashOFollowedByPath(_) => Some(n),
        _ => None,
    });

    if let Some(output_path_index) = output_path_index {
        let output_path = match &args[output_path_index] {
            StripArg::DashOPath(path) | StripArg::DashOFollowedByPath(path) => path.clone(),
            _ => unreachable!(),
        };

        let input_path_index = args.iter().enumerate().find_map(|(n, arg)| match arg {
            StripArg::InputPath(_) => Some(n),
            _ => None,
        });
        let input_path_index = input_path_index.ok_or_eyre("input path not specified")?;
        let input_path = match &args[input_path_index] {
            StripArg::InputPath(path) => path.clone(),
            _ => unreachable!(),
        };

        let mut input = std::fs::File::open(&input_path)
            .with_context(|| format!("failed to open {}", input_path.display()))?;
        let extracted = brioche_pack::extract_pack(&mut input);

        if let Ok(extracted) = extracted {
            let all_resource_dirs = brioche_resources::find_resource_dirs(&input_path, true)
                .with_context(|| {
                    format!("failed to get resource dirs for {}", input_path.display())
                })?;
            let source_path =
                brioche_autopack::pack_source(&input_path, &extracted.pack, &all_resource_dirs)
                    .with_context(|| {
                        format!("failed to get source path for {}", input_path.display())
                    })?;

            match source_path {
                brioche_autopack::PackSource::This => {
                    input.rewind()?;
                    let mut unpacked_input = input.take(extracted.unpacked_len.try_into()?);

                    let mut temp_file = tempfile::NamedTempFile::new()?;
                    std::io::copy(&mut unpacked_input, &mut temp_file)?;

                    args[input_path_index] = StripArg::InputPath(temp_file.path().to_path_buf());
                    args.remove(output_path_index);

                    remapped_files.push(RemapFile::Inject {
                        temp_file,
                        output_path,
                        pack: extracted.pack,
                    });
                }
                brioche_autopack::PackSource::Path(source_path) => {
                    let mut source = std::fs::File::open(&source_path)?;

                    let mut temp_file = tempfile::NamedTempFile::new()?;
                    std::io::copy(&mut source, &mut temp_file)?;

                    args[input_path_index] = StripArg::InputPath(temp_file.path().to_path_buf());
                    args.remove(output_path_index);

                    remapped_files.push(RemapFile::UpdateSource {
                        input_path,
                        extracted,
                        temp_file,
                        output_path,
                    });
                }
            }
        }
    } else {
        for arg in args.iter_mut() {
            match arg {
                StripArg::Arg(_) => {}
                StripArg::DashOPath(_) | StripArg::DashOFollowedByPath(_) => unreachable!(),
                StripArg::InputPath(path) => {
                    let mut input = std::fs::File::open(&path)
                        .with_context(|| format!("failed to open {}", path.display()))?;
                    let extracted = brioche_pack::extract_pack(&mut input);

                    if let Ok(extracted) = extracted {
                        let all_resource_dirs = brioche_resources::find_resource_dirs(path, true)
                            .with_context(|| {
                            format!("failed to get resource dirs for {}", path.display())
                        })?;
                        let source_path = brioche_autopack::pack_source(
                            path,
                            &extracted.pack,
                            &all_resource_dirs,
                        )
                        .with_context(|| {
                            format!("failed to get source path for {}", path.display())
                        })?;

                        match source_path {
                            brioche_autopack::PackSource::This => {
                                input.rewind()?;
                                let mut unpacked_input =
                                    input.take(extracted.unpacked_len.try_into()?);

                                let mut temp_file = tempfile::NamedTempFile::new()?;
                                std::io::copy(&mut unpacked_input, &mut temp_file)?;

                                let output_path =
                                    std::mem::replace(path, temp_file.path().to_path_buf());

                                remapped_files.push(RemapFile::Inject {
                                    output_path,
                                    temp_file,
                                    pack: extracted.pack,
                                });
                            }
                            brioche_autopack::PackSource::Path(source_path) => {
                                let mut source_path = std::fs::File::open(&source_path)?;

                                let mut temp_file = tempfile::NamedTempFile::new()?;
                                std::io::copy(&mut source_path, &mut temp_file)?;

                                let input_output_path =
                                    std::mem::replace(path, temp_file.path().to_path_buf());

                                remapped_files.push(RemapFile::UpdateSource {
                                    input_path: input_output_path.clone(),
                                    extracted,
                                    temp_file,
                                    output_path: input_output_path,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn finish_remapped_file(remapped_file: RemapFile) -> eyre::Result<()> {
    match remapped_file {
        RemapFile::Inject {
            pack,
            mut temp_file,
            output_path,
        } => {
            let mut output = std::fs::File::create(output_path).with_context(|| {
                format!("failed to open output {}", temp_file.path().display(),)
            })?;

            temp_file.rewind()?;
            std::io::copy(&mut temp_file, &mut output)?;

            brioche_pack::inject_pack(&mut output, &pack)?;
        }
        RemapFile::UpdateSource {
            input_path,
            extracted,
            mut temp_file,
            output_path,
        } => {
            let input_resource_dirs = brioche_resources::find_resource_dirs(&input_path, true)?;
            let output_resource_dir = brioche_resources::find_output_resource_dir(&output_path)?;

            let new_pack = match extracted.pack {
                brioche_pack::Pack::LdLinux {
                    program,
                    interpreter,
                    library_dirs,
                    runtime_library_dirs,
                } => {
                    let program = program.to_path().map_err(|_| {
                        eyre::eyre!("invalid program path: {}", bstr::BStr::new(&program))
                    })?;
                    let program_name = program
                        .file_name()
                        .ok_or_eyre("could not get program name from path")?;
                    let program_name = std::path::Path::new(program_name);

                    let program_path =
                        brioche_resources::find_in_resource_dirs(&input_resource_dirs, program)
                            .ok_or_eyre("could not find program in resource dirs")?;
                    let program_metadata = std::fs::metadata(&program_path)
                        .context("could not get program metadata")?;
                    let is_executable = is_executable(&program_metadata.permissions());

                    temp_file.rewind()?;
                    let new_source_resource = brioche_resources::add_named_blob(
                        &output_resource_dir,
                        &mut temp_file,
                        is_executable,
                        program_name,
                    )?;

                    let new_source_resource = <Vec<u8>>::from_path_buf(new_source_resource)
                        .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))?;

                    brioche_pack::Pack::LdLinux {
                        program: new_source_resource,
                        interpreter,
                        library_dirs,
                        runtime_library_dirs,
                    }
                }
                brioche_pack::Pack::Static { .. } | brioche_pack::Pack::Metadata { .. } => {
                    eyre::bail!("unsupported pack to update source: {:#?}", extracted.pack);
                }
            };

            let unpacked_len: u64 = extracted.unpacked_len.try_into()?;
            match (input_path.canonicalize(), output_path.canonicalize()) {
                (Ok(input_path), Ok(output_path)) if input_path == output_path => {
                    let mut output = std::fs::OpenOptions::new()
                        .read(true)
                        .append(true)
                        .open(&output_path)?;

                    output.set_len(unpacked_len)?;
                    output.seek(std::io::SeekFrom::Start(unpacked_len))?;
                    brioche_pack::inject_pack(&mut output, &new_pack)?;
                }
                _ => {
                    let input = std::fs::File::open(&input_path)?;
                    let mut input_unpacked = input.take(unpacked_len);
                    let mut output = std::fs::File::create(output_path)?;

                    std::io::copy(&mut input_unpacked, &mut output)?;
                    brioche_pack::inject_pack(&mut output, &new_pack)?;
                }
            }
        }
    }

    Ok(())
}

pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    permissions.mode() & 0o100 != 0
}
