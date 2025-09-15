// based on: https://github.com/NixOS/nixpkgs/blob/master/pkgs/stdenv/generic/make-derivation.nix # commit/d3afbb6da92399220987b8fbb1165c4a2f1a7b5c
use clap::{Arg, Command};
use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command as SysCommand,
};
use walkdir::WalkDir;
use anyhow::{Result, bail};

fn main() -> Result<()> {
    let matches = Command::new("patchShebangs")
        .about("Patches script interpreter paths")
        .arg(Arg::new("host").long("host").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("build").long("build").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("update").long("update").action(clap::ArgAction::SetTrue))
        .arg(Arg::new("paths").num_args(1..).required(true))
        .get_matches();

    let update = matches.get_flag("update");
    let use_host_path = matches.get_flag("host");

    let path_env = if use_host_path {
        env::var("HOST_PATH").unwrap_or_default()
    } else {
        env::var("PATH").unwrap_or_default()
    };

    let paths: Vec<&String> = matches.get_many::<String>("paths").unwrap().collect();
    println!("Patching script interpreter paths in {:?}", paths);

    for path in paths {
        patch_shebangs_in_path(path, &path_env, update)?;
    }

    Ok(())
}

fn patch_shebangs_in_path<P: AsRef<Path>>(path: P, path_env: &str, update: bool) -> Result<()> {
    for entry in WalkDir::new(path) {
        let entry = entry?;
        let file_path = entry.path();

        // Only regular executable files
        if !entry.file_type().is_file() || entry.metadata()?.permissions().mode() & 0o100 == 0 {
            continue;
        }

        if let Some(new_interpreter) = process_file(file_path, path_env, update)? {
            println!("{}: shebang updated to {}", file_path.display(), new_interpreter);
        }
    }
    Ok(())
}

fn process_file(path: &Path, path_env: &str, update: bool) -> Result<Option<String>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();

    let bytes_read = reader.read_line(&mut first_line)?;
    if bytes_read == 0 || !first_line.starts_with("#!") {
        return Ok(None); // not a shebang script
    }

    let original_shebang = first_line.trim_end().to_string();
    let shebang_content = original_shebang.trim_start_matches("#!").trim();

    let mut parts = shebang_content.split_whitespace();
    let interpreter = parts.next().unwrap_or("");
    let mut args: Vec<&str> = parts.collect();

    let new_interpreter_line = if interpreter.ends_with("/env") {
        // Handle env shebang
        if let Some(first_arg) = args.first() {
            if *first_arg == "-S" {
                args.remove(0);
                if args.is_empty() {
                    bail!("Invalid -S usage in shebang: {}", original_shebang);
                }
                let prog = args.remove(0);
                let prog_path = which_in_path(prog, path_env)?;
                let env_path = which_in_path("env", path_env)?;
                format!("#!{} -S {} {}", env_path, prog_path, args.join(" "))
            } else if first_arg.starts_with('-') || first_arg.contains('=') {
                bail!("Unsupported env usage in shebang: {}", original_shebang);
            } else {
                let prog_path = which_in_path(first_arg, path_env)?;
                format!("#!{}", prog_path)
            }
        } else {
            bail!("Invalid env usage in shebang: {}", original_shebang);
        }
    } else {
        // Regular interpreter
        let base = Path::new(interpreter)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(interpreter);

        let resolved = which_in_path(base, path_env)?;
        let all_args = std::iter::once(resolved.as_str()).chain(args.iter().copied()).collect::<Vec<_>>();
        format!("#!{}", all_args.join(" "))
    };

    if original_shebang != new_interpreter_line {
        if update || !interpreter.starts_with("/nix/store") {
            // Read full content
            let content = fs::read_to_string(path)?;
            let updated = content.replacen(&original_shebang, &new_interpreter_line, 1);

            // Preserve timestamp
            let metadata = fs::metadata(path)?;
            let mtime = filetime::FileTime::from_last_modification_time(&metadata);

            fs::write(path, updated)?;
            filetime::set_file_mtime(path, mtime)?;

            return Ok(Some(new_interpreter_line));
        }
    }

    Ok(None)
}

fn which_in_path(program: &str, path_env: &str) -> Result<String> {
    let paths = env::split_paths(path_env);
    for dir in paths {
        let full_path = dir.join(program);
        if full_path.exists() && full_path.is_file() {
            return Ok(full_path.to_string_lossy().to_string());
        }
    }
    bail!("Could not find {} in given path", program);
}
