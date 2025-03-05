use std::{collections::HashSet, path::PathBuf, process::ExitCode};

use bstr::ByteSlice as _;
use eyre::{Context as _, OptionExt as _};

enum Mode {
    AutopackEnabled {
        output_path: PathBuf,
        resource_dir: PathBuf,
        all_resource_dirs: Vec<PathBuf>,
    },
    AutopackDisabled,
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

    let mut output_path = Some(PathBuf::from("a.out"));
    let mut library_search_paths = vec![];
    let mut input_paths = vec![];

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = <[u8]>::from_os_str(&arg).ok_or_eyre("invalid arg")?;
        let arg = bstr::BStr::new(arg);

        if &**arg == b"-o" {
            let output = args.next().ok_or_eyre("invalid arg")?;
            output_path = Some(PathBuf::from(output));
        } else if let Some(output) = arg.strip_prefix(b"-o") {
            let output = output.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
            output_path = Some(output.to_path_buf());
        } else if &**arg == b"-L" {
            let lib_path = args.next().ok_or_eyre("invalid arg")?;
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
        } else {
            let input_path = arg.to_path().map_err(|_| eyre::eyre!("invalid path"))?;
            input_paths.push(input_path.to_owned());
        }
    }

    // `ld` can take dynamic libraries directly as inputs, so check all the
    // input paths when searching for required libraries
    library_search_paths.extend(input_paths);

    // Determine whether we will pack the resulting binary or not. We do this
    // before running the command so we can bail early if the resource dir
    // cannot be found.
    let autopack_mode = std::env::var("BRIOCHE_LD_AUTOPACK");
    let autopack_mode = match (autopack_mode.as_deref(), output_path) {
        (Ok("false"), _) | (_, None) => Mode::AutopackDisabled,
        (_, Some(output_path)) => {
            let resource_dir = brioche_resources::find_output_resource_dir(&output_path)
                .context("error while finding resource dir")?;
            let all_resource_dirs = brioche_resources::find_resource_dirs(&current_exe, true)
                .context("error while finding resource dir")?;
            Mode::AutopackEnabled {
                output_path,
                resource_dir,
                all_resource_dirs,
            }
        }
    };
    let skip_unknown_libs = matches!(
        std::env::var("BRIOCHE_LD_AUTOPACK_SKIP_UNKNOWN_LIBS").as_deref(),
        Ok("true")
    );

    let mut command = std::process::Command::new(&linker);
    command.args(std::env::args_os().skip(1));
    let status = command.status()?;

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
            // We already wrote the binary, so nothing to do
        }
    };

    Ok(ExitCode::SUCCESS)
}
