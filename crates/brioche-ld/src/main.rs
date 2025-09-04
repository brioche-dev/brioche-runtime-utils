use std::{
    collections::{HashSet, VecDeque},
    io::Read,
    path::PathBuf,
    process::ExitCode,
};

use bstr::{ByteSlice as _, ByteVec as _};
use chumsky::prelude::*;
use eyre::{Context as _, OptionExt as _};

/// The maximum number of times we allow reading `@file` arguments. This is
/// a simple measure we use to avoid reading cyclic `@file` references forever.
const MAX_FILE_DEREFERENCES: u32 = 5000;

#[derive(Debug)]
enum Mode {
    AutopackEnabled {
        output_path: PathBuf,
        resource_dir: PathBuf,
        all_resource_dirs: Vec<PathBuf>,
    },
    AutopackDisabled,
}

fn main() -> ExitCode {
    log::debug!("starting brioche-ld");

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
    let current_exe_name = current_exe
        .file_name()
        .ok_or_eyre("failed to get current executable name")?;
    let current_exe_dir = current_exe
        .parent()
        .ok_or_eyre("failed to get current executable dir")?;
    let current_exe_parent_dir = current_exe_dir
        .parent()
        .ok_or_eyre("failed to get current executable dir")?;
    let ld_resource_dir = current_exe_parent_dir.join("libexec").join("brioche-ld");
    if !ld_resource_dir.is_dir() {
        eyre::bail!(
            "failed to find linker resource dir: {}",
            ld_resource_dir.display()
        );
    }

    let linker = ld_resource_dir.join(current_exe_name);
    let packed_path = ld_resource_dir.join("brioche-packed");

    log::info!("using linker: {}", linker.display());
    log::debug!("using packed path: {}", packed_path.display());

    let mut output_path = Some(PathBuf::from("a.out"));
    let mut library_search_paths = vec![];
    let mut input_paths = vec![];

    let mut file_dereferences = 0;

    let mut args: VecDeque<_> = std::env::args_os().skip(1).collect();
    while let Some(arg) = args.pop_front() {
        let arg = <[u8]>::from_os_str(&arg).ok_or_eyre("invalid arg")?;
        let arg = bstr::BStr::new(arg);

        log::trace!("arg: {arg:?}");

        if &**arg == b"-o" {
            let output = args.pop_front().ok_or_eyre("invalid arg")?;
            output_path = Some(PathBuf::from(output));
        } else if let Some(output) = arg.strip_prefix(b"-o") {
            let output = output.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
            output_path = Some(output.to_path_buf());
        } else if &**arg == b"-L" {
            let lib_path = args.pop_front().ok_or_eyre("invalid arg")?;
            library_search_paths.push(PathBuf::from(lib_path));
        } else if let Some(lib_path) = arg.strip_prefix(b"-L") {
            let lib_path = lib_path
                .to_path()
                .map_err(|_| eyre::eyre!("invalid path"))?;
            library_search_paths.push(lib_path.to_owned());
        } else if &**arg == b"--help" || &**arg == b"--version" || &**arg == b"-v" {
            // Skip packing if we're just showing help or version info
            output_path = None;
        } else if arg.starts_with(b"-") {
            // Ignore other arguments
        } else if let Some(arg_file_path) = arg.strip_prefix(b"@") {
            log::trace!("dereferencing arg: {:?}", bstr::BStr::new(arg_file_path));

            let arg_file_path = arg_file_path
                .to_path()
                .map_err(|_| eyre::eyre!("invalid path"))?;

            // `@file` arg. Arguments are parsed and read from `file`
            file_dereferences += 1;
            if file_dereferences > MAX_FILE_DEREFERENCES {
                eyre::bail!("encountered more than {MAX_FILE_DEREFERENCES} '@file' arguments");
            }

            let mut file_contents = Vec::<u8>::new();
            let mut file = std::fs::File::open(arg_file_path).wrap_err_with(|| {
                format!("failed to read args from path {}", arg_file_path.display())
            })?;
            file.read_to_end(&mut file_contents)?;
            drop(file);

            // Parse the arguments from the file
            let args_from_file = file_args_parser()
                .parse(&file_contents[..])
                .into_result()
                .map_err(|error| {
                    eyre::eyre!(
                        "failed to parse args from path {}: {error:#?}",
                        arg_file_path.display()
                    )
                })?;

            // Add each parsed arg to the start of `args`, so they will get
            // processed in place of the `@file` arg
            for new_arg in args_from_file.into_iter().rev() {
                let new_arg = Vec::from(new_arg).into_os_string()?;
                args.push_front(new_arg);
            }
        } else {
            let input_path = arg.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
            input_paths.push(input_path.to_owned());
        }
    }

    log::trace!("output path: {output_path:?}");
    for input_path in &input_paths {
        log::trace!("input path: {}", input_path.display());
    }
    for library_search_path in &library_search_paths {
        log::trace!("library search path: {}", library_search_path.display());
    }

    // `ld` can take dynamic libraries directly as inputs, so check all the
    // input paths when searching for required libraries
    library_search_paths.extend(input_paths);

    let autopack_mode = std::env::var("BRIOCHE_LD_AUTOPACK");
    let skip_unknown_libs = matches!(
        std::env::var("BRIOCHE_LD_AUTOPACK_SKIP_UNKNOWN_LIBS").as_deref(),
        Ok("true")
    );
    let include_globs = std::env::var("BRIOCHE_LD_AUTOPACK_INCLUDE")
        .ok()
        .map(|globs| {
            let mut globset = globset::GlobSetBuilder::new();

            let globs = globs.split(';').filter(|glob| glob.is_empty());
            for glob in globs {
                let glob = globset::Glob::new(glob)
                    .context("invalid glob in $BRIOCHE_LD_AUTOPACK_EXCLUDE")?;
                globset.add(glob);
            }

            let globset = globset.build()?;
            eyre::Ok(globset)
        })
        .transpose()?;
    let exclude_globs = std::env::var("BRIOCHE_LD_AUTOPACK_EXCLUDE")
        .ok()
        .map(|globs| {
            let mut globset = globset::GlobSetBuilder::new();

            let globs = globs.split(';').filter(|glob| glob.is_empty());
            for glob in globs {
                let glob = globset::Glob::new(glob)
                    .context("invalid glob in $BRIOCHE_LD_AUTOPACK_EXCLUDE")?;
                globset.add(glob);
            }

            let globset = globset.build()?;
            eyre::Ok(globset)
        })
        .transpose()?;

    // Determine whether we will pack the resulting binary or not. We do this
    // before running the command so we can bail early if the resource dir
    // cannot be found.
    let autopack_mode = match (autopack_mode.as_deref(), output_path) {
        (Ok("false"), _) | (_, None) => Mode::AutopackDisabled,
        (_, Some(output_path)) => {
            let should_include = match (include_globs, exclude_globs) {
                (Some(include_globs), _) => include_globs.is_match(&output_path),
                (None, Some(exclude_globs)) => !exclude_globs.is_match(&output_path),
                (None, None) => true,
            };

            if should_include {
                let resource_dir = brioche_resources::find_output_resource_dir(&output_path)
                    .context("error while finding resource dir")?;
                let all_resource_dirs = brioche_resources::find_resource_dirs(&current_exe, true)
                    .context("error while finding resource dir")?;
                Mode::AutopackEnabled {
                    output_path,
                    resource_dir,
                    all_resource_dirs,
                }
            } else {
                log::info!(
                    "not autopacking {} (excluded by glob patterns)",
                    output_path.display()
                );
                Mode::AutopackDisabled
            }
        }
    };

    log::debug!("autopack_mode: {autopack_mode:?}");
    log::debug!("skip unknown libs: {skip_unknown_libs}");

    let mut command = std::process::Command::new(&linker);
    command.args(std::env::args_os().skip(1));
    let status = command.status()?;

    log::info!("linker returned {status:?}");

    if !status.success() {
        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from);
        return Ok(exit_code);
    }

    match autopack_mode {
        Mode::AutopackEnabled {
            output_path,
            resource_dir,
            all_resource_dirs,
        } => {
            log::info!("autopacking: {}", output_path.display());

            let dynamic_linking_config = brioche_autopack::DynamicLinkingConfig {
                library_paths: library_search_paths,
                skip_libraries: HashSet::new(),
                extra_libraries: vec![],
                skip_unknown_libraries: skip_unknown_libs,
            };
            brioche_autopack::autopack(&brioche_autopack::AutopackConfig {
                resource_dir,
                all_resource_dirs,
                inputs: brioche_autopack::AutopackInputs::Paths(vec![output_path]),
                quiet: true,
                link_dependencies: vec![ld_resource_dir],
                dynamic_binary: Some(brioche_autopack::DynamicBinaryConfig {
                    packed_executable: packed_path,
                    extra_runtime_library_paths: vec![],
                    dynamic_linking: dynamic_linking_config.clone(),
                }),
                shared_library: Some(brioche_autopack::SharedLibraryConfig {
                    dynamic_linking: dynamic_linking_config,
                    allow_empty: true,
                }),
                repack: None,
                script: None,
            })?;
        }
        Mode::AutopackDisabled => {
            log::info!("autopacking disabled");
            // We already wrote the binary, so nothing to do
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn file_args_parser<'a>() -> impl Parser<'a, &'a [u8], Vec<bstr::BString>, extra::Err<Rich<'a, u8>>>
{
    let escape = just(b'\\').ignore_then(any());
    let bare_arg = none_of(b"\\\"' \t\r\n\x0C")
        .or(escape)
        .repeated()
        .at_least(1)
        .collect();
    let double_quoted_arg = none_of(b"\\\"")
        .or(escape)
        .repeated()
        .collect()
        .delimited_by(just(b'"'), just(b'"'));
    let single_quoted_arg = none_of(b"\\'")
        .or(escape)
        .repeated()
        .collect()
        .delimited_by(just(b'\''), just(b'\''));
    let arg = bare_arg
        .or(double_quoted_arg)
        .or(single_quoted_arg)
        .padded()
        .map(bstr::BString::new);
    arg.repeated().collect()
}

#[cfg(test)]
mod tests {
    use chumsky::Parser as _;

    use crate::file_args_parser;

    const EMPTY_OUTPUT: Vec<Vec<u8>> = vec![];

    #[test]
    fn test_file_args_parser() {
        let parser = file_args_parser();
        assert_eq!(parser.parse(b"").unwrap(), EMPTY_OUTPUT);
        assert_eq!(parser.parse(b"foo").unwrap(), ["foo"]);
        assert_eq!(parser.parse(b"foo bar baz").unwrap(), ["foo", "bar", "baz"]);
        assert_eq!(parser.parse(b"\"\"").unwrap(), [b""]);
        assert_eq!(parser.parse(b"''").unwrap(), [b""]);
        assert_eq!(
            parser
                .parse(b"a \"bcd'ef'\\\\\\\"gh\" \r\n 'ijk\\'' \"lmn \t opq\\\"\"")
                .unwrap(),
            ["a", "bcd'ef'\\\"gh", "ijk'", "lmn \t opq\""],
        );
        assert_eq!(
            parser.parse(b"a \x00\xFF b").unwrap(),
            [&b"a"[..], &b"\x00\xFF"[..], &b"b"[..]],
        );

        assert!(parser.parse(b"\\").has_errors());
        assert!(parser.parse(b"\"").has_errors());
        assert!(parser.parse(b"'").has_errors());
    }
}
