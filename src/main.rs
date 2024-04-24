mod check;
mod cli;
mod github;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let mut args = std::env::args();
    let cmd = args.next();
    let subcommand = args.next();
    if Some("server") == subcommand.as_deref() {
        github::hook_server().await;
    } else if Some("check") == subcommand.as_deref() {
        cli::main(args.next().unwrap());
    } else {
        show_help(&cmd.unwrap_or("typst-package-check".to_owned()));
    }
}

fn show_help(program: &str) {
    println!("Usage :");
    println!("  {program} server");
    println!("    Start a server to handle GitHub webhooks and report checks in pull requests.");
    println!("  {program} check PACKAGE:VERSION");
    println!("    Check a local package at the specified version");
}
