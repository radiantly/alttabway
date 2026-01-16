use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use futures_util::{sink::SinkExt, stream::StreamExt};
use rkyv::{Archive, Deserialize, Serialize, rancor};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::instrument;
#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum IpcCommand {
    Ping,
    Show,
    Hide,
}

#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum IpcCommandResponse {
    Success,
    Error(String),
}

pub struct AlttabwayIpc {}

impl AlttabwayIpc {
    #[instrument]
    fn get_socket_path() -> Result<PathBuf> {
        let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR")?;

        let mut socket_dir_path = PathBuf::from(format!("{}/alttabway", xdg_runtime_dir));

        // create directory if it does not exist
        let _ = fs::create_dir(&socket_dir_path);

        socket_dir_path.push(".socket.sock");
        Ok(socket_dir_path)
    }

    async fn handle_connection(stream: UnixStream, tx: UnboundedSender<IpcCommand>) -> Result<()> {
        let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

        while let Some(result) = framed.next().await {
            let Ok(bytes) = result else { continue };

            let response =
                if let Ok(command) = rkyv::from_bytes::<IpcCommand, rancor::Error>(&bytes) {
                    tracing::trace!("IPC RECEIVED");
                    tx.send(command)?;
                    IpcCommandResponse::Success
                } else {
                    IpcCommandResponse::Error(
                        "Unrecognized IPC command. Try reloading the alttabway daemon?".into(),
                    )
                };

            let response_bytes = rkyv::to_bytes::<rancor::Error>(&response)?;
            framed.send(response_bytes.into_vec().into()).await?;
        }
        Ok(())
    }

    async fn listen(listener: UnixListener, tx: UnboundedSender<IpcCommand>) -> Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            let tx_copy = tx.clone();
            tokio::spawn(Self::handle_connection(stream, tx_copy));
        }
    }

    async fn send_socket_command(
        socket_path: &PathBuf,
        command: IpcCommand,
    ) -> Result<IpcCommandResponse> {
        let stream = UnixStream::connect(&socket_path).await?;

        let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

        framed
            .send(rkyv::to_bytes::<rancor::Error>(&command)?.into_vec().into())
            .await?;

        let bytes = framed
            .next()
            .await
            .context("stream closed without response?")??;

        let response = rkyv::from_bytes::<IpcCommandResponse, rancor::Error>(&bytes)?;
        Ok(response)
    }

    #[instrument]
    pub async fn start_server() -> Result<UnboundedReceiver<IpcCommand>> {
        let socket_path = Self::get_socket_path()?;
        tracing::info!("path {:?}", socket_path);

        if let Ok(_) = Self::send_socket_command(&socket_path, IpcCommand::Ping).await {
            bail!("Another instance is already running.");
        }

        let _ = fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)?;

        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(Self::listen(listener, tx));

        Ok(rx)
    }

    pub async fn send_command(command: IpcCommand) -> Result<IpcCommandResponse> {
        let socket_path = Self::get_socket_path()?;

        Self::send_socket_command(&socket_path, command).await
    }
}
