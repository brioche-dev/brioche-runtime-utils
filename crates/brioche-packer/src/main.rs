use std::{os::unix::fs::OpenOptionsExt as _, path::PathBuf, process::ExitCode};

use clap::Parser;
use eyre::{Context as _, OptionExt as _};

mod autowrap;

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
    Autowrap(AutowrapArgs),
    Read {
        program: PathBuf,
    },
}

impl std::str::FromStr for AutowrapTemplateValue {
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
                autowrap::template::TemplateVariableValue::Path(value)
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
        Args::Autowrap(args) => {
            run_autowrap(args)?;
        }
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let pack = brioche_pack::extract_pack(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &pack)?;
            println!();
        }
    }

    Ok(())
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Parser)]
struct AutowrapArgs {
    #[arg(long)]
    schema: bool,

    #[arg(required_unless_present = "schema")]
    recipe_path: Option<PathBuf>,

    #[arg(long, required_unless_present = "schema")]
    config: Option<String>,

    #[arg(long = "var", value_parser)]
    variables: Vec<AutowrapTemplateValue>,
}

#[derive(Debug, Clone)]
struct AutowrapTemplateValue {
    name: String,
    value: autowrap::template::TemplateVariableValue,
}

fn run_autowrap(args: AutowrapArgs) -> eyre::Result<()> {
    if args.schema {
        let schema = schemars::schema_for!(autowrap::template::AutowrapConfigTemplate);
        serde_json::to_writer_pretty(std::io::stdout().lock(), &schema)?;
        println!();
        return Ok(());
    }

    let recipe_path = args.recipe_path.ok_or_eyre("missing RECIPE_PATH")?;
    let config = args.config.ok_or_eyre("missing --config")?;

    let config_template =
        serde_json::from_str::<autowrap::template::AutowrapConfigTemplate>(&config);
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

    let ctx = &autowrap::template::AutowrapConfigTemplateContext {
        variables,
        resource_dir,
    };
    let config = config_template.build(ctx, recipe_path)?;

    autowrap::autowrap(&config)?;

    Ok(())
}
