use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

#[derive(Debug, Parser)]
enum Args {
    MakeRunnable {
        #[arg(long)]
        runnable: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        runnable_data: String,
    },
    Read {
        program: PathBuf,
    },
}

fn main() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), RunnableError> {
    let args = Args::parse();

    match args {
        Args::MakeRunnable {
            runnable,
            output,
            runnable_data,
        } => {
            let runnable_data =
                serde_json::from_str(&runnable_data).map_err(RunnableError::DeserializeRunnable)?;

            std::fs::copy(runnable, &output)?;
            let mut output = std::fs::OpenOptions::new().append(true).open(&output)?;

            runnable_core::inject(&mut output, &runnable_data)?;
        }
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let runnable_data = runnable_core::extract(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &runnable_data)
                .map_err(RunnableError::SerializeRunnable)?;
            println!();
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum RunnableError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("error deserializing runnable data: {0}")]
    DeserializeRunnable(#[source] serde_json::Error),
    #[error("error serializing runnable data: {0}")]
    SerializeRunnable(#[source] serde_json::Error),
    #[error(transparent)]
    InjectRunnable(#[from] runnable_core::InjectRunnableError),
    #[error(transparent)]
    ExtractRunnable(#[from] runnable_core::ExtractRunnableError),
}
