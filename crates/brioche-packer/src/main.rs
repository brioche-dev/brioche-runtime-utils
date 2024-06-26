use std::{os::unix::fs::OpenOptionsExt as _, path::PathBuf, process::ExitCode};

use clap::Parser;

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
    Autowrap(autowrap::AutowrapArgs),
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
        Args::Autowrap(args) => autowrap::autowrap(&args)?,
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let pack = brioche_pack::extract_pack(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &pack)?;
            println!();
        }
    }

    Ok(())
}
