mod cli;
mod daemon;
mod engine;
mod manifest;
mod paths;
mod protocol;
mod runtime;
mod store;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run(std::env::args_os().skip(1))
}
