use std::{os::unix::ffi::OsStrExt, os::unix::process::CommandExt as _, process::ExitCode};

use eyre::{Context as _, OptionExt as _};

fn main() -> ExitCode {
    let result = run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("brioche-cc error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> eyre::Result<()> {
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
    let cc_resource_dir = current_exe_parent_dir.join("libexec").join("brioche-cc");
    if !cc_resource_dir.is_dir() {
        eyre::bail!(
            "failed to find c resource dir: {}",
            cc_resource_dir.display()
        );
    }

    let cc = cc_resource_dir.join(current_exe_name);
    let sysroot_path = cc_resource_dir
        .join("sysroot")
        .canonicalize()
        .context("failed to get sysroot path from 'libexec/brioche-cc/sysroot'")?;

    let mut args = std::env::args_os();
    let first_arg = args.next();
    let next_args = args.collect::<Vec<_>>();

    let mut command = std::process::Command::new(&cc);
    if let Some(arg0) = first_arg {
        command.arg0(&arg0);
    }

    let has_sysroot_arg = next_args.iter().any(|arg| {
        let bytes = arg.as_bytes();
        bytes == b"--sysroot" || bytes.starts_with(b"--sysroot=")
    });

    if !has_sysroot_arg {
        command.arg("--sysroot").arg(sysroot_path);
    }

    command.args(&next_args);

    let error = command.exec();
    panic!("brioche-cc exec error: {error:#}");
}
