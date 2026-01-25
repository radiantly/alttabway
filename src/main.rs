use alttabway::{
    daemon::Daemon,
    ipc::{AlttabwayIpc, Direction, IpcCommand, Modifier},
};
use clap::{ArgGroup, Parser, Subcommand};

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
    #[command(group(ArgGroup::new("direction").args(["next", "previous"])))]
    Show {
        /// If already visible, navigate to the next option
        #[arg(long)]
        next: bool,

        /// If already visible, navigate to the previous option
        #[arg(long)]
        previous: bool,

        /// Modifier keys that need to be held for the window to be shown
        #[arg(long, value_enum, default_values_t = Daemon::DEFAULT_REQ_MODIFIER, value_delimiter = ',')]
        modifiers_held: Vec<Modifier>,
    },
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
        Commands::Show {
            next,
            previous,
            modifiers_held,
        } => {
            let direction = if *next {
                Some(Direction::Next)
            } else if *previous {
                Some(Direction::Previous)
            } else {
                None
            };

            tracing::debug!("Modifiers required to be held: {:?}", modifiers_held);

            match AlttabwayIpc::send_command(IpcCommand::Show {
                direction,
                modifiers: modifiers_held.clone(),
            })
            .await
            {
                Ok(response) => tracing::info!("{:?}", response),
                Err(err) => tracing::warn!(
                    "Please check if the alttabway daemon is running. Error: {}",
                    err
                ),
            }
        }
    }
}
