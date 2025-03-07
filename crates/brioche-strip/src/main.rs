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
            Self::Arg(arg) => Ok(vec![arg]),
            Self::DashOPath(path) => {
                let path_bytes = <[u8]>::from_path(&path).ok_or_eyre("invalid path")?;

                let mut arg = b"-o".to_vec();
                arg.extend_from_slice(path_bytes);
                Ok(vec![arg.to_os_str()?.to_owned()])
            }
            Self::DashOFollowedByPath(path) => Ok(vec![
                std::ffi::OsString::from("-o"),
                std::ffi::OsString::from(path),
            ]),
            Self::InputPath(path) => Ok(vec![std::ffi::OsString::from(path)]),
        }
    }
}

fn main() -> ExitCode {
    let result = run();

    match result {
        Ok(exit_code) => exit_code,
        Err(err) => {
            eprintln!("{err:#}");
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
    if std::env::var("BRIOCHE_STRIP_AUTOPACK").as_deref() == Ok("false") {
        let mut command = std::process::Command::new(strip);
        command.args(std::env::args_os().skip(1));
        let status = command.status()?;

        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from);
        return Ok(exit_code);
    }

    let mut args = std::env::args_os().skip(1);
    let mut strip_args = vec![];

    // Parse each argument
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
                // These are the (known) arguments that take an extra
                // parameter

                let next_arg = args.next().ok_or_eyre("expected arg after flag")?;
                strip_args.extend([StripArg::Arg(arg), StripArg::Arg(next_arg)]);
            }
            b"-o" => {
                // Parse the next argument as the output path
                let output = args.next().ok_or_eyre("expected path after -o")?;
                let output = std::path::PathBuf::from(output);
                strip_args.push(StripArg::DashOFollowedByPath(output));
            }
            _ => {
                if let Some(output) = arg_bytes.strip_prefix(b"-o") {
                    // Support "-o<path>" syntax
                    let output = output.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
                    strip_args.push(StripArg::DashOPath(output.to_path_buf()));
                } else if arg_bytes.starts_with(b"-") {
                    // Pass through any extra argument starting with a "-"
                    strip_args.push(StripArg::Arg(arg));
                } else if arg_bytes.starts_with(b"@") {
                    // @ is used to parse extra args from a file
                    // (not yet implemented)
                    eyre::bail!("using @ for passing args is not supported");
                } else {
                    // Other args are treated as input files
                    let input_path = arg_bytes
                        .to_path()
                        .map_err(|_| eyre::eyre!("invalid path"))?;
                    strip_args.push(StripArg::InputPath(input_path.to_owned()));
                }
            }
        }
    }

    // Remap args and files so we can strip them while preserving packs
    let mut remapped_files = vec![];
    remap_files(&mut strip_args, &mut remapped_files)?;

    // Convert the remapped args back into an argument list
    let strip_args = strip_args
        .into_iter()
        .map(StripArg::into_args)
        .collect::<eyre::Result<Vec<_>>>()?;

    // Call the original strip process
    let mut command = std::process::Command::new(strip);
    command.args(strip_args.iter().flatten());
    let status = command.status()?;

    if !status.success() {
        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from);
        return Ok(exit_code);
    }

    // Finish processing each file we remapped
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
    let mut output_path_indices = args.iter().enumerate().filter_map(|(n, arg)| match arg {
        StripArg::DashOPath(_) | StripArg::DashOFollowedByPath(_) => Some(n),
        _ => None,
    });
    let output_path_index = output_path_indices.next();
    eyre::ensure!(
        output_path_indices.next().is_none(),
        "-o argument specified multiple times"
    );

    if let Some(output_path_index) = output_path_index {
        // Output path was specified. There should be one input path and
        // one output path argument

        let output_path = match &args[output_path_index] {
            StripArg::DashOPath(path) | StripArg::DashOFollowedByPath(path) => path.clone(),
            _ => unreachable!(),
        };

        // Get the input path
        let mut input_path_indices = args.iter().enumerate().filter_map(|(n, arg)| match arg {
            StripArg::InputPath(_) => Some(n),
            _ => None,
        });
        let input_path_index = input_path_indices.next();
        eyre::ensure!(
            input_path_indices.next().is_none(),
            "multiple input paths specified with -o"
        );

        let input_path_index = input_path_index.ok_or_eyre("input path not specified")?;
        let input_path = match &args[input_path_index] {
            StripArg::InputPath(path) => path.clone(),
            _ => unreachable!(),
        };

        // Try to extract a pack from the input path
        let mut input = std::fs::File::open(&input_path)
            .with_context(|| format!("failed to open {}", input_path.display()))?;
        let extracted = brioche_pack::extract_pack(&mut input);

        if let Ok(extracted) = extracted {
            // If the input is a packed file, we need to remap it

            // Get the source path for the pack
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
                    // The pack was appended to the original file. In this
                    // case, we should copy this file without the pack, strip
                    // it, then re-add the same pack

                    // Copy the unpacked part of the input to a temp file
                    input.rewind()?;
                    let mut unpacked_input = input.take(extracted.unpacked_len.try_into()?);
                    let mut temp_file = tempfile::NamedTempFile::new()?;
                    std::io::copy(&mut unpacked_input, &mut temp_file)?;

                    // Replace the input and output path args with just
                    // the new temporary path
                    args[input_path_index] = StripArg::InputPath(temp_file.path().to_path_buf());
                    args.remove(output_path_index);

                    // After processing, copy the updated temp file to the
                    // output then inject the pack
                    remapped_files.push(RemapFile::Inject {
                        temp_file,
                        output_path,
                        pack: extracted.pack,
                    });
                }
                brioche_autopack::PackSource::Path(source_path) => {
                    // The pack refers to a different source file

                    // Copy the source file to a new temp file
                    let mut source = std::fs::File::open(&source_path)?;
                    let mut temp_file = tempfile::NamedTempFile::new()?;
                    std::io::copy(&mut source, &mut temp_file)?;

                    // Replace the input and output path args with just
                    // the new temporary path
                    args[input_path_index] = StripArg::InputPath(temp_file.path().to_path_buf());
                    args.remove(output_path_index);

                    // After processing, copy the packed input file to
                    // the output path, then replace the source file
                    // with the temp file
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
        // No output path specified. In this case, each file should
        // effectively be updated in-place

        for arg in args.iter_mut() {
            match arg {
                StripArg::Arg(_) => {
                    // Pass through normal arguments
                }
                StripArg::DashOPath(_) | StripArg::DashOFollowedByPath(_) => {
                    // We already know there was no output path specified
                    unreachable!();
                }
                StripArg::InputPath(path) => {
                    // Try to extract a pack from the input path
                    let mut input = std::fs::File::open(&path)
                        .with_context(|| format!("failed to open {}", path.display()))?;
                    let extracted = brioche_pack::extract_pack(&mut input);

                    if let Ok(extracted) = extracted {
                        // If the input is a packed file, we need to remap it

                        // Get the source path for the pack
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
                                // The pack was appended to the original file. In this
                                // case, we should copy this file without the pack, strip
                                // it, then re-add the same pack

                                // Copy the unpacked part of the input to a temp file
                                input.rewind()?;
                                let mut unpacked_input =
                                    input.take(extracted.unpacked_len.try_into()?);
                                let mut temp_file = tempfile::NamedTempFile::new()?;
                                std::io::copy(&mut unpacked_input, &mut temp_file)?;

                                // Replace the input path argument with
                                // the temp path
                                let original_path =
                                    std::mem::replace(path, temp_file.path().to_path_buf());

                                // After processing, copy the temp file
                                // over the original path, then inject
                                // the pack
                                remapped_files.push(RemapFile::Inject {
                                    output_path: original_path,
                                    temp_file,
                                    pack: extracted.pack,
                                });
                            }
                            brioche_autopack::PackSource::Path(source_path) => {
                                // The pack refers to a different source file

                                // Copy the source file to a new temp file
                                let mut source_path = std::fs::File::open(&source_path)?;
                                let mut temp_file = tempfile::NamedTempFile::new()?;
                                std::io::copy(&mut source_path, &mut temp_file)?;

                                // Replace the input path argument with
                                // the temp path
                                let original_path =
                                    std::mem::replace(path, temp_file.path().to_path_buf());

                                // After processing, update the source path in
                                // the input path to use the updated temp file
                                remapped_files.push(RemapFile::UpdateSource {
                                    input_path: original_path.clone(),
                                    extracted,
                                    temp_file,
                                    output_path: original_path,
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
            // Open the output file
            let mut output = std::fs::File::create(output_path).with_context(|| {
                format!("failed to open output {}", temp_file.path().display(),)
            })?;

            // Copy the temp file to the output
            temp_file.rewind()?;
            std::io::copy(&mut temp_file, &mut output)?;

            // Inject the pack into the output
            brioche_pack::inject_pack(&mut output, &pack)?;
        }
        RemapFile::UpdateSource {
            input_path,
            extracted,
            mut temp_file,
            output_path,
        } => {
            // Get the resource dirs
            let input_resource_dirs = brioche_resources::find_resource_dirs(&input_path, true)?;
            let output_resource_dir = brioche_resources::find_output_resource_dir(&output_path)?;

            let new_pack = match extracted.pack {
                brioche_pack::Pack::LdLinux {
                    program,
                    interpreter,
                    library_dirs,
                    runtime_library_dirs,
                } => {
                    // Get the original program name
                    let program = program.to_path().map_err(|_| {
                        eyre::eyre!("invalid program path: {}", bstr::BStr::new(&program))
                    })?;
                    let program_name = program
                        .file_name()
                        .ok_or_eyre("could not get program name from path")?;
                    let program_name = std::path::Path::new(program_name);

                    // Determine if the original program was executable
                    let program_path =
                        brioche_resources::find_in_resource_dirs(&input_resource_dirs, program)
                            .ok_or_eyre("could not find program in resource dirs")?;
                    let program_metadata = std::fs::metadata(&program_path)
                        .context("could not get program metadata")?;
                    let is_executable = is_executable(&program_metadata.permissions());

                    // Add the temp file as a new resource. We re-use the
                    // original program's name and permissions
                    temp_file.rewind()?;
                    let new_source_resource = brioche_resources::add_named_blob(
                        &output_resource_dir,
                        &mut temp_file,
                        is_executable,
                        program_name,
                    )?;
                    let new_source_resource = <Vec<u8>>::from_path_buf(new_source_resource)
                        .map_err(|_| eyre::eyre!("invalid UTF-8 in path"))?;

                    // Re-use the same details from the pack, but with the
                    // new resource created from the temp file
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
                    // If the input path and output path are the same, we
                    // remove the pack by truncating, then inject the new pack

                    // Open the file
                    let mut output = std::fs::OpenOptions::new()
                        .read(true)
                        .append(true)
                        .open(&output_path)?;

                    // Truncate the file to remove the old pack
                    output.set_len(unpacked_len)?;

                    // Inject the new pack
                    output.seek(std::io::SeekFrom::Start(unpacked_len))?;
                    brioche_pack::inject_pack(&mut output, &new_pack)?;
                }
                _ => {
                    // If the input and output paths are different, we
                    // copy the unpacked part of the input to the output
                    // then inject the new pack

                    let input = std::fs::File::open(&input_path)?;
                    let mut output = std::fs::File::create(output_path)?;

                    // Copy the unpacked part of the input to the output
                    let mut input_unpacked = input.take(unpacked_len);
                    std::io::copy(&mut input_unpacked, &mut output)?;

                    // Inject the new pack
                    brioche_pack::inject_pack(&mut output, &new_pack)?;
                }
            }
        }
    }

    Ok(())
}

#[must_use]
pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    permissions.mode() & 0o100 != 0
}
