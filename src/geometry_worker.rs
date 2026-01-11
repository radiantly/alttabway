use crate::geometry_provider::GeometryProvider;
use crate::{geometry_ipc::HyprlandIpc, geometry_provider::Geometry};
use anyhow::{Result, bail};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub type GeometryRequestId = u64;

enum GeometryWorkerRequestEvent {
    ActiveWindow(GeometryRequestId),
}

pub enum GeometryWorkerEvent {
    ActiveWindow(GeometryRequestId, Geometry),
}

#[derive(Debug)]
pub struct GeometryWorker {
    request_counter: GeometryRequestId,
    request_tx: UnboundedSender<GeometryWorkerRequestEvent>,
}

impl GeometryWorker {
    pub fn new() -> Result<(Self, UnboundedReceiver<GeometryWorkerEvent>)> {
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
                    GeometryWorkerRequestEvent::ActiveWindow(request_id) => {
                        if let Ok(geometry) = provider.get_active_window_geometry()
                            && response_tx
                                .send(GeometryWorkerEvent::ActiveWindow(request_id, geometry))
                                .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                request_counter: 0,
                request_tx,
            },
            response_rx,
        ))
    }

    pub fn request_active_window_geometry(&mut self) -> Result<GeometryRequestId> {
        self.request_counter += 1;
        self.request_tx
            .send(GeometryWorkerRequestEvent::ActiveWindow(
                self.request_counter,
            ))?;
        Ok(self.request_counter)
    }
}
