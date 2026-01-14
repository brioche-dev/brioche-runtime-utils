#![cfg(target_os = "linux")]

use core::ffi::CStr;
use std::ffi::CString;

use bstr::ByteSlice as _;

const BRIOCHE_PACKED_ERROR: u8 = 121;

unsafe extern "C" {
    static environ: *const *const libc::c_char;
}

#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::missing_safety_doc,
    clippy::similar_names
)]
pub unsafe fn entrypoint(argc: libc::c_int, argv: *const *const libc::c_char) -> libc::c_int {
    let mut args = vec![];
    let mut env_vars = vec![];

    let argc: usize = argc.try_into().unwrap_or(0);
    for n in 0..argc {
        let arg = unsafe { *argv.add(n) };

        if arg.is_null() {
            break;
        }

        let arg = unsafe { CStr::from_ptr(arg) };
        args.push(arg);
    }

    for n in 0.. {
        let var = unsafe { *environ.add(n) };

        if var.is_null() {
            break;
        }

        let var = unsafe { CStr::from_ptr(var) };
        env_vars.push(var);
    }

    let result = run(&args, &env_vars);
    match result {
        Ok(()) => libc::EXIT_SUCCESS,
        Err(err) => {
            eprintln!("brioche-packed error: {err}");
            BRIOCHE_PACKED_ERROR.into()
        }
    }
}

fn run(args: &[&CStr], env_vars: &[&CStr]) -> Result<(), PackedError> {
    let path = std::env::current_exe()?;
    let parent_path = path.parent().ok_or(PackedError::InvalidPath)?;
    let resource_dirs = brioche_resources::find_resource_dirs(&path, true)?;
    let mut program = std::fs::File::open(&path)?;
    let extracted = brioche_pack::extract_pack(&mut program)?;

    match extracted.pack {
        brioche_pack::Pack::LdLinux {
            program,
            interpreter,
            library_dirs,
            runtime_library_dirs,
        } => {
            let interpreter = interpreter
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let interpreter = brioche_resources::find_in_resource_dirs(&resource_dirs, interpreter)
                .ok_or(PackedError::ResourceNotFound)?;

            let program = program.to_path().map_err(|_| PackedError::InvalidPath)?;
            let program = brioche_resources::find_in_resource_dirs(&resource_dirs, program)
                .ok_or(PackedError::ResourceNotFound)?;
            let program = program.canonicalize()?;
            let mut exec = userland_execve::ExecOptions::new(&interpreter);

            let interpreter = <[u8]>::from_path(&interpreter).ok_or(PackedError::InvalidPath)?;
            let interpreter = CString::new(interpreter).map_err(|_| PackedError::InvalidPath)?;

            let mut resolved_library_dirs = vec![];

            for library_dir in &runtime_library_dirs {
                let library_dir = library_dir
                    .to_path()
                    .map_err(|_| PackedError::InvalidPath)?;
                let resolved_library_dir = parent_path.join(library_dir);
                resolved_library_dirs.push(resolved_library_dir);
            }

            for library_dir in &library_dirs {
                let library_dir = library_dir
                    .to_path()
                    .map_err(|_| PackedError::InvalidPath)?;
                let library_dir =
                    brioche_resources::find_in_resource_dirs(&resource_dirs, library_dir)
                        .ok_or(PackedError::ResourceNotFound)?;
                resolved_library_dirs.push(library_dir);
            }

            // Add argv0
            exec.arg(interpreter);

            if !resolved_library_dirs.is_empty() {
                let mut ld_library_path = bstr::BString::default();
                for (n, library_dir) in resolved_library_dirs.iter().enumerate() {
                    if n > 0 {
                        ld_library_path.push(b':');
                    }

                    let path = <[u8]>::from_path(library_dir).ok_or(PackedError::InvalidPath)?;
                    ld_library_path.extend(path);
                }

                if let Some(env_library_path) = std::env::var_os("LD_LIBRARY_PATH") {
                    let env_library_path =
                        <[u8]>::from_os_str(&env_library_path).ok_or(PackedError::InvalidPath)?;
                    if !env_library_path.is_empty() {
                        ld_library_path.push(b':');
                        ld_library_path.extend(env_library_path);
                    }
                }

                exec.arg(c"--library-path");

                let ld_library_path =
                    CString::new(ld_library_path).map_err(|_| PackedError::InvalidPath)?;
                exec.arg(ld_library_path);
            }

            let mut args = args.iter();
            if let Some(arg0) = args.next() {
                exec.arg(c"--argv0");
                exec.arg(arg0);
            }

            let program = <[u8]>::from_path(&program).ok_or(PackedError::InvalidPath)?;
            let program = CString::new(program).map_err(|_| PackedError::InvalidPath)?;
            exec.arg(program);

            exec.args(args);

            exec.env_pairs(env_vars);

            userland_execve::exec_with_options(exec);
        }
        brioche_pack::Pack::Static { .. } => {
            unimplemented!("execution of a static executable");
        }
        brioche_pack::Pack::Metadata { .. } => {
            unimplemented!("execution of a metadata pack");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PackedError {
    IoError(#[from] std::io::Error),
    ExtractPackError(#[from] brioche_pack::ExtractPackError),
    PackResourceDirError(#[from] brioche_resources::PackResourceDirError),
    InvalidPath,
    ResourceNotFound,
}

impl core::fmt::Display for PackedError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(error_summary(self))
    }
}

const fn error_summary(error: &PackedError) -> &'static str {
    match error {
        PackedError::IoError(_) => "io error",
        PackedError::ExtractPackError(error) => match error {
            brioche_pack::ExtractPackError::ReadPackedProgramError(_) => {
                "failed to read packed program: io error"
            }
            brioche_pack::ExtractPackError::MarkerNotFound => {
                "marker not found at the end of the packed program"
            }
            brioche_pack::ExtractPackError::MalformedMarker => {
                "malformed marker at the end of the packed program"
            }
            brioche_pack::ExtractPackError::InvalidPack(_) => "failed to parse pack: bincode error",
            brioche_pack::ExtractPackError::TryFromIntError(_) => "integer conversion error",
        },
        PackedError::PackResourceDirError(error) => match error {
            brioche_resources::PackResourceDirError::NotFound => {
                "brioche pack resource dir not found"
            }
            brioche_resources::PackResourceDirError::DepthLimitReached => {
                "reached depth limit while searching for brioche pack resource dir"
            }
            brioche_resources::PackResourceDirError::IoError(_) => {
                "error while searching for brioche pack resource dir: io error"
            }
        },
        PackedError::InvalidPath => "invalid path",
        PackedError::ResourceNotFound => "resource not found",
    }
}
