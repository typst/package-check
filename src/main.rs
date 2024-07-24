mod check;
mod cli;
mod github;
mod world;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    if std::env::var("LOG_STYLE").as_deref().unwrap_or("human") == "json" {
        tracing_subscriber::fmt()
            .event_format(tracing_subscriber::fmt::format::json())
            .init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let mut args = std::env::args();
    let cmd = args.next();
    let subcommand = args.next();
    if Some("server") == subcommand.as_deref() {
        github::hook_server().await;
    } else if Some("check") == subcommand.as_deref() {
        cli::main(args.next().unwrap_or_default()).await;
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
