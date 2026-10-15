mod config;
mod containers;
mod fields;
mod filter;
mod help;
mod layout;
mod legacy_fields;
mod logs;
mod queue;
mod scripts;
mod shell;
mod terminal;
mod theme;
mod util;

use std::env;
use util::Result;

fn run() -> Result<i32> {
    let mut args: Vec<String> = env::args_os()
        .skip(1)
        .map(|s| {
            s.into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())
        })
        .collect::<Result<_>>()?;
    // Permit queue/config options before an explicit command as well as after it.
    let mut prefix = 0;
    while let Some(arg) = args.get(prefix) {
        if arg == "--no-view-filter" {
            prefix += 1;
            continue;
        }
        let key = arg.split('=').next().unwrap_or(arg);
        if !matches!(key, "--config" | "--view" | "--color" | "--filter") {
            break;
        }
        prefix += if arg.contains('=') { 1 } else { 2 };
    }
    if prefix > 0
        && args
            .get(prefix)
            .is_some_and(|s| matches!(s.as_str(), "q" | "config"))
    {
        let command = args.remove(prefix);
        args.insert(0, command);
    }
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => help::run(&[]),
        Some("help") => help::run(&args[1..]),
        Some("--version" | "-V") => {
            util::print(concat!("sj ", env!("CARGO_PKG_VERSION"), "\n"))?;
            Ok(0)
        }
        Some("q") => {
            args.remove(0);
            queue::run(args)
        }
        Some("config") => {
            args.remove(0);
            config::run(args)
        }
        Some("out") => {
            args.remove(0);
            logs::run(args)
        }
        Some("script") => {
            args.remove(0);
            scripts::run(args)
        }
        Some("sh") => {
            args.remove(0);
            shell::run(args)
        }
        Some("__node") => {
            args.remove(0);
            shell::node_run(args)
        }
        Some(arg) if !arg.starts_with('-') => {
            Err(format!("unknown command {arg:?}; use sj --help"))
        }
        _ => queue::run(args),
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("sj: {error}");
            std::process::exit(1);
        }
    }
}
