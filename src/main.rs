mod cli;
mod tui;

use clap::Parser;

fn main() {
    // Let `skills list | head` end quietly instead of panicking on a closed pipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args = cli::Cli::parse();
    let json = args.json;
    let result = if args.command.is_none() {
        args.workspace(false).and_then(tui::run)
    } else {
        cli::run(args)
    };
    if let Err(e) = result {
        if json {
            eprintln!("{}", serde_json::json!({"error": format!("{e:#}")}));
        } else {
            eprintln!("error: {e:#}");
        }
        std::process::exit(1);
    }
}
