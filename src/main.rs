mod cli;
mod tui;

use clap::Parser;

fn main() {
    let args = cli::Cli::parse();
    let json = args.json;
    let result = if args.command.is_none() {
        tui::run(args.root.as_deref())
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
