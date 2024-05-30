const RUNNABLE_ERROR: u8 = 122;

fn main() -> std::process::ExitCode {
    let result = run();

    match result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("{}", error);
            std::process::ExitCode::from(RUNNABLE_ERROR)
        }
    }
}

fn run() -> Result<std::process::ExitCode, RunnableError> {
    let current_exe_path = std::env::current_exe()?;
    let current_exe_dir = current_exe_path
        .parent()
        .ok_or(RunnableError::InvalidExecutablePath)?;

    let current_exe = std::fs::File::open(&current_exe_path)?;
    let runnable = runnable_core::extract(current_exe)?;

    let command = runnable.command.to_os_string(current_exe_dir)?;
    let mut command = std::process::Command::new(command);

    for arg in runnable.args {
        let arg = arg.to_os_string(current_exe_dir)?;
        command.arg(arg);
    }

    for (key, value) in runnable.env {
        let value = value.to_os_string(current_exe_dir)?;
        command.env(key, value);
    }

    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::os::unix::process::CommandExt as _;

            let error = command.exec();
            Err(error.into())
        } else {
            let status = command.status()?;
            let exit_code = status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(RUNNABLE_ERROR);
            Ok(std::process::ExitCode::from(exit_code))
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RunnableError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Invalid executable path")]
    InvalidExecutablePath,
    #[error(transparent)]
    ExtractError(#[from] runnable_core::ExtractRunnableError),
    #[error(transparent)]
    RunnableTemplateError(#[from] runnable_core::RunnableTemplateError),
}
