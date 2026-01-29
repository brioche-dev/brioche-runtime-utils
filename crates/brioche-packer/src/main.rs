use std::{
    ffi::OsStr,
    io::Seek as _,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::Parser;
use eyre::{Context as _, OptionExt as _};

mod autopack_template;

#[derive(Debug, Parser)]
enum Args {
    Pack {
        #[arg(long)]
        packed: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        pack: String,
    },
    Autopack(AutopackArgs),
    Read {
        program: PathBuf,
    },
    SourcePath {
        program: PathBuf,
    },
    UpdateSource(UpdateSourceArgs),
}

impl std::str::FromStr for AutopackTemplateValue {
    type Err = eyre::Error;

    fn from_str(s: &str) -> eyre::Result<Self> {
        let (name, value) = s
            .split_once('=')
            .ok_or_else(|| eyre::eyre!("expected `<NAME>=<TYPE>:<VALUE>` format"))?;
        let (ty, value) = value
            .split_once(':')
            .ok_or_else(|| eyre::eyre!("expected `<NAME>=<TYPE>:<VALUE>` format"))?;

        let value = match ty {
            "path" => {
                let value = PathBuf::from(value);
                autopack_template::TemplateVariableValue::Path(value)
            }
            _ => {
                eyre::bail!("unknown type {ty:?}, expected \"path\"");
            }
        };

        Ok(Self {
            name: name.to_string(),
            value,
        })
    }
}

fn main() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> eyre::Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    match args {
        Args::Pack {
            packed,
            output,
            pack,
        } => {
            let pack = serde_json::from_str(&pack)?;

            let mut packed = std::fs::File::open(packed)?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o777)
                .open(output)?;

            std::io::copy(&mut packed, &mut output)?;

            brioche_pack::inject_pack(&mut output, &pack)?;
        }
        Args::Autopack(args) => {
            run_autopack(args)?;
        }
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let extracted = brioche_pack::extract_pack(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &extracted.pack)?;
            println!();
        }
        Args::SourcePath {
            program: program_path,
        } => {
            let mut program = std::fs::File::open(&program_path)?;
            let extracted = brioche_pack::extract_pack(&mut program)?;
            let all_resource_dirs = brioche_resources::find_resource_dirs(&program_path, true)?;

            let source_path =
                brioche_autopack::pack_source(&program_path, &extracted.pack, &all_resource_dirs)
                    .with_context(|| {
                    format!("failed to get source path for {}", program_path.display())
                })?;

            match source_path {
                brioche_autopack::PackSource::This => {
                    println!("{}", program_path.display());
                }
                brioche_autopack::PackSource::Path(path) => {
                    println!("{}", path.display());
                }
            }
        }
        Args::UpdateSource(args) => {
            run_update_source(&args)?;
        }
    }

    Ok(())
}

#[derive(Debug, Parser)]
struct AutopackArgs {
    #[arg(long)]
    schema: bool,

    #[arg(required_unless_present = "schema")]
    recipe_path: Option<PathBuf>,

    #[arg(long, required_unless_present = "schema")]
    config: Option<String>,

    #[arg(long = "var", value_parser)]
    variables: Vec<AutopackTemplateValue>,
}

#[derive(Debug, Clone)]
struct AutopackTemplateValue {
    name: String,
    value: autopack_template::TemplateVariableValue,
}

fn run_autopack(args: AutopackArgs) -> eyre::Result<()> {
    if args.schema {
        let schema = schemars::schema_for!(autopack_template::AutopackConfigTemplate);
        serde_json::to_writer_pretty(std::io::stdout().lock(), &schema)?;
        println!();
        return Ok(());
    }

    let recipe_path = args.recipe_path.ok_or_eyre("missing RECIPE_PATH")?;
    let config = args.config.ok_or_eyre("missing --config")?;

    let config_template =
        serde_json::from_str::<autopack_template::AutopackConfigTemplate>(&config);
    let config_template = match config_template {
        Ok(config_template) => config_template,
        Err(err) => {
            return Err(err)
                .context("failed to parse config template (pass --schema to show schema)");
        }
    };

    let variables = args
        .variables
        .into_iter()
        .map(|variable| (variable.name, variable.value))
        .collect();

    // HACK: Workaround because finding a resource dir takes a program
    // path rather than a directory path, but then gets the parent path
    let program = recipe_path.join("program");

    let resource_dir = brioche_resources::find_output_resource_dir(&program)?;

    let ctx = &autopack_template::AutopackConfigTemplateContext {
        variables,
        resource_dir,
    };
    let config = config_template.build(ctx, &recipe_path)?;

    brioche_autopack::autopack(&config)?;

    Ok(())
}

#[derive(Debug, Parser)]
struct UpdateSourceArgs {
    program: PathBuf,
    #[arg(long)]
    new_source: PathBuf,
    #[arg(long)]
    name: Option<String>,
}

fn run_update_source(args: &UpdateSourceArgs) -> eyre::Result<()> {
    let program = std::fs::File::open(&args.program)?;
    let extracted = brioche_pack::extract_pack(program)?;
    let output_resource_dir = brioche_resources::find_output_resource_dir(&args.program)?;

    let (new_pack, unpacked_len) = match extracted.pack {
        brioche_pack::Pack::LdLinux {
            program,
            interpreter,
            library_dirs,
            runtime_library_dirs,
        } => {
            let program_path = Path::new(OsStr::from_bytes(&program));
            let program_name = program_path
                .file_name()
                .ok_or_eyre("could not get program name from path")?;
            let new_name = args
                .name
                .as_deref()
                .map_or_else(|| Path::new(program_name), Path::new);

            let new_source = std::fs::File::open(&args.new_source).map_err(|_| {
                eyre::eyre!("could not open new source {}", args.new_source.display())
            })?;

            let new_source_permissions = new_source.metadata()?.permissions();
            let is_executable = is_executable(&new_source_permissions);

            let new_source_reader = std::io::BufReader::new(new_source);
            let new_source_resource = brioche_resources::add_named_blob(
                &output_resource_dir,
                new_source_reader,
                is_executable,
                new_name,
            )?;
            let new_source_resource = new_source_resource.into_os_string().into_vec();

            let new_pack = brioche_pack::Pack::LdLinux {
                program: new_source_resource,
                interpreter,
                library_dirs,
                runtime_library_dirs,
            };
            (new_pack, Some(extracted.unpacked_len))
        }
        brioche_pack::Pack::Static { library_dirs } => {
            let pack = brioche_pack::Pack::Static { library_dirs };

            if args.new_source.canonicalize()? != args.program.canonicalize()? {
                std::fs::copy(&args.new_source, &args.program)?;
            }

            let program = std::fs::File::open(&args.program)?;
            let new_source_extracted = brioche_pack::extract_pack(program);

            if let Ok(new_source_extracted) = new_source_extracted {
                (pack, Some(new_source_extracted.unpacked_len))
            } else {
                (pack, None)
            }
        }
        brioche_pack::Pack::Metadata { format, .. } => {
            // No metadata formats can be updated currently
            eyre::bail!("unsupported metadata format: {format:?}");
        }
    };

    let mut program = std::fs::OpenOptions::new()
        .append(true)
        .open(&args.program)?;
    if let Some(unpacked_len) = unpacked_len {
        program.set_len(unpacked_len.try_into()?)?;
        program.seek(std::io::SeekFrom::End(0))?;
    }

    brioche_pack::inject_pack(&mut program, &new_pack)?;

    Ok(())
}

#[must_use]
pub fn is_executable(permissions: &std::fs::Permissions) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    permissions.mode() & 0o100 != 0
}

pub fn without_pack(
    mut contents: impl std::io::Read + std::io::Seek,
) -> eyre::Result<impl std::io::Read> {
    let content_length = contents.seek(std::io::SeekFrom::End(0))?;
    contents.rewind()?;

    if let Ok(extracted) = brioche_pack::extract_pack(&mut contents) {
        Ok(contents.take(extracted.unpacked_len.try_into()?))
    } else {
        Ok(contents.take(content_length))
    }
}
