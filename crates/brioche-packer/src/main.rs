use std::{
    collections::HashMap,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::ExitCode,
};

use bstr::ByteVec as _;
use clap::Parser;
use eyre::{Context as _, OptionExt as _};

mod autopack_template;
mod pack_runnable_template;

#[allow(clippy::large_enum_variant)]
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
    PackRunnable(PackRunnableArgs),
    Read {
        program: PathBuf,
    },
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
        Args::PackRunnable(args) => {
            run_pack_runnable(args)?;
        }
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let extracted = brioche_pack::extract_pack(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &extracted.pack)?;
            println!();
        }
    }

    Ok(())
}

#[allow(clippy::large_enum_variant)]
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
    let config = config_template.build(ctx, recipe_path)?;

    brioche_autopack::autopack(&config)?;

    Ok(())
}

#[derive(Debug, Parser)]
struct PackRunnableArgs {
    #[arg(long)]
    schema: bool,

    #[arg(long, required_unless_present = "schema")]
    packed: Option<PathBuf>,

    #[arg(long, required_unless_present = "schema")]
    output: Option<PathBuf>,

    #[arg(long, required_unless_present = "schema")]
    config: Option<String>,

    #[arg(long = "resource", value_parser)]
    resources: Vec<ResourceValue>,
}

fn run_pack_runnable(args: PackRunnableArgs) -> eyre::Result<()> {
    if args.schema {
        let schema = schemars::schema_for!(pack_runnable_template::PackRunnableTemplate);
        serde_json::to_writer_pretty(std::io::stdout().lock(), &schema)?;
        println!();
        return Ok(());
    }

    let packed = args.packed.ok_or_eyre("missing --packed")?;
    let output = args.output.ok_or_eyre("missing --output")?;
    let config = args.config.ok_or_eyre("missing --config")?;

    let resource_dir = brioche_resources::find_output_resource_dir(&output)?;

    let mut resources = HashMap::new();
    for resource in args.resources {
        let metadata = std::fs::metadata(&resource.path)
            .with_context(|| format!("failed to get metadata for resource {:?}", resource.name))?;
        let resource_path = if metadata.is_dir() {
            brioche_resources::add_resource_directory(&resource_dir, &resource.path)?
        } else {
            let file = std::fs::File::open(&resource.path)
                .with_context(|| format!("failed to open resource {:?}", resource.name))?;
            let name = resource.path.file_name().ok_or_else(|| {
                eyre::eyre!("failed to get file name for resource {:?}", resource.name)
            })?;
            let name = Path::new(name);

            let metadata = file
                .metadata()
                .context("failed to get metadata for resource")?;
            let is_executable = metadata.permissions().mode() & 0o111 != 0;

            brioche_resources::add_named_blob(&resource_dir, file, is_executable, name)?
        };

        let resource_path = <Vec<u8>>::from_path_buf(resource_path).map_err(|resource_path| {
            eyre::eyre!("failed to convert resource path {resource_path:?}")
        })?;
        resources.insert(resource.name, resource_path);
    }

    let config_template =
        serde_json::from_str::<pack_runnable_template::PackRunnableTemplate>(&config);
    let config_template = match config_template {
        Ok(config_template) => config_template,
        Err(err) => {
            return Err(err)
                .context("failed to parse config template (pass --schema to show schema)");
        }
    };

    let ctx = &pack_runnable_template::PackRunnableContext { resources };
    let metadata = config_template.build(ctx)?;
    let metadata = serde_json::to_vec(&metadata)?;
    let pack = brioche_pack::Pack::Metadata {
        resource_paths: ctx.resource_paths(),
        format: runnable_core::FORMAT.into(),
        metadata,
    };

    let mut packed = std::fs::File::open(packed)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o777)
        .open(output)?;

    std::io::copy(&mut packed, &mut output)?;
    brioche_pack::inject_pack(&mut output, &pack)?;

    Ok(())
}

#[derive(Debug, Clone)]
struct AutopackTemplateValue {
    name: String,
    value: autopack_template::TemplateVariableValue,
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

#[derive(Debug, Clone)]
struct ResourceValue {
    name: String,
    path: PathBuf,
}

impl std::str::FromStr for ResourceValue {
    type Err = eyre::Error;

    fn from_str(s: &str) -> eyre::Result<Self> {
        let (name, path) = s
            .split_once('=')
            .ok_or_else(|| eyre::eyre!("expected `<NAME>=<PATH>` format"))?;

        Ok(Self {
            name: name.to_string(),
            path: path.into(),
        })
    }
}
