use alttabway::{
    daemon::Daemon,
    ipc::{AlttabwayIpc, IpcCommand},
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the alttabway daemon
    Daemon,

    /// Show the alt-tab window (requires daemon to be running)
    Show,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level cmd
    match &cli.command {
        Commands::Daemon => {
            tracing::debug!("requesting daemon start");
            if let Err(err) = Daemon::start().await {
                tracing::info!("Exiting: {}", err);
            }
        }
        Commands::Show => match AlttabwayIpc::send_command(IpcCommand::Show).await {
            Ok(response) => tracing::info!("{:?}", response),
            Err(err) => tracing::warn!(
                "Please check if the alttabway daemon is running. Error: {}",
                err
            ),
        },
    }
}
