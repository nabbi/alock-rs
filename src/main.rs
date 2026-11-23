mod args;
mod auth;
mod background;
mod cursor;
mod font;
mod keys;
mod lock;
mod present;
mod render;
mod theme;
mod ui;

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|a| a == auth::HELPER_FLAG) {
        return auth::helper_main(&argv[1..]);
    }
    auth::harden();
    match args::parse(&argv) {
        Ok(args::Parsed::Exit(msg)) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Ok(args::Parsed::Run(cfg)) => match lock::run(cfg) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("alock-rs: {e}");
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("alock-rs: {e}");
            ExitCode::from(2)
        }
    }
}
