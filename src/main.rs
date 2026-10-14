mod containers;
mod help;
mod logs;
mod queue;
mod scripts;
mod shell;
mod terminal;
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
