use crate::geometry_provider::GeometryProvider;
use crate::{geometry_ipc::HyprlandIpc, geometry_provider::Geometry};
use anyhow::{Result, bail};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

enum GeometryWorkerRequestEvent<U: Copy + Send + 'static> {
    ActiveWindow(U),
}

#[derive(Debug)]
pub enum GeometryWorkerEvent<U: Copy + Send + 'static> {
    ActiveWindow(U, Geometry),
}

#[derive(Debug)]
pub struct GeometryWorker<U: Copy + Send + 'static> {
    request_tx: UnboundedSender<GeometryWorkerRequestEvent<U>>,
    response_rx: UnboundedReceiver<GeometryWorkerEvent<U>>,
}

impl<U: Copy + Send + 'static> GeometryWorker<U> {
    pub fn new() -> Result<Self> {
        let mut provider: Box<dyn GeometryProvider + Send> = if let Ok(ipc) = HyprlandIpc::new() {
            Box::new(ipc)
        } else {
            bail!("no geometry provider");
        };

        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let (response_tx, response_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(event) = request_rx.recv().await {
                match event {
                    GeometryWorkerRequestEvent::ActiveWindow(user_data) => {
                        if let Ok(geometry) = provider.get_active_window_geometry()
                            && response_tx
                                .send(GeometryWorkerEvent::ActiveWindow(user_data, geometry))
                                .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });

        Ok(Self {
            request_tx,
            response_rx,
        })
    }

    pub fn request_active_window_geometry(&mut self, user_data: U) -> Result<()> {
        let result = self
            .request_tx
            .send(GeometryWorkerRequestEvent::ActiveWindow(user_data));

        if result.is_err() {
            bail!("failed to send. geometry worker is down.")
        }

        Ok(())
    }

    pub async fn recv(&mut self) -> Option<GeometryWorkerEvent<U>> {
        self.response_rx.recv().await
    }
}
