use alttabway::daemon::Daemon;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Daemon,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level cmd
    match &cli.command {
        Commands::Daemon => {
            info!("requesting daemon start");
            if let Err(err) = Daemon::start().await {
                tracing::error!("{:?}", err);
            }
        }
    }
}
