mod check;
mod cli;
mod github;
mod world;

fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let mut args = std::env::args();
    let cmd = args.next();
    let subcommand = args.next();
    if Some("server") == subcommand.as_deref() {
        github::hook_server();
    } else if Some("check") == subcommand.as_deref() {
        cli::main(args.next().unwrap_or_default());
    } else {
        show_help(&cmd.unwrap_or("typst-package-check".to_owned()));
    }
}

fn show_help(program: &str) {
    println!("Usage :");
    println!("  {program} server");
    println!("    Start a server to handle GitHub webhooks and report checks in pull requests.");
    println!("  {program} check @preview/PACKAGE:VERSION");
    println!(
        "    Check a local package at the specified version. To be run in typst/packages/packages."
    );
    println!("  {program} check");
    println!("    Check the package in the current directory.");
}
