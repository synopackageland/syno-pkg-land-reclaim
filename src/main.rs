use clap::Parser;
use syno_reclaim::app;
use syno_reclaim::cli::Cli;
use syno_reclaim::root_entry;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(start) = root_entry::invocation(std::env::var_os(root_entry::MODE_ENV), &args) {
        std::process::exit(root_entry::start(start));
    }
    std::process::exit(app::run(Cli::parse()));
}
