use std::{os::unix::process::CommandExt as _, process::ExitCode};

use eyre::{Context as _, OptionExt as _};

fn main() -> ExitCode {
    let result = run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("brioche-cc error: {:#}", err);
            ExitCode::FAILURE
        }
    }
}

fn run() -> eyre::Result<()> {
    let current_exe = std::env::current_exe().context("failed to get current executable")?;
    let current_exe_name = current_exe
        .file_name()
        .ok_or_eyre("failed to get current executable name")?;
    let sysroot = current_exe
        .parent()
        .and_then(|dir| dir.parent())
        .ok_or_eyre("failed to get sysroot path")?;

    let mut original_exe_name = current_exe_name.to_owned();
    original_exe_name.push("-orig");
    let original_exe = current_exe.with_file_name(&original_exe_name);

    let mut args = std::env::args_os();
    let arg0 = args.next();
    let args = args.collect::<Vec<_>>();

    let mut command = std::process::Command::new(&original_exe);
    if let Some(arg0) = arg0 {
        command.arg0(&arg0);
    }

    let has_sysroot_arg = args.iter().any(|arg| {
        let arg_string = arg.to_string_lossy();
        arg_string == "--sysroot" || arg_string.starts_with("--sysroot=")
    });

    if !has_sysroot_arg {
        command.arg("--sysroot").arg(sysroot);
    }

    command.args(&args);

    let error = command.exec();
    panic!("brioche-cc exec error: {error:#}");
}
