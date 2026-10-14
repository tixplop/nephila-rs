use clap::Parser;
use rkg::{cli, error};

fn main() {
    let args = cli::Cli::parse();
    if let Err(e) = cli::run(args) {
        error::report_error(&e);
        std::process::exit(e.exit_code());
    }
}
