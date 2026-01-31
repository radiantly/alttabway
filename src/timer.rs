use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::time;

#[derive(Debug)]
pub struct Timer {
    rx: Receiver<()>,
    internal_tx: Sender<Duration>,
}

impl Timer {
    pub fn new(period: Duration) -> Self {
        let (tx, rx) = mpsc::channel(1);
        let (internal_tx, mut internal_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            let mut wait_for = period;
            loop {
                match time::timeout(wait_for, internal_rx.recv()).await {
                    Ok(Some(duration)) => wait_for = duration,
                    Ok(None) => break, // other side has closed channel
                    Err(_) => match tx.send(()).await {
                        Ok(_) => wait_for = period,
                        Err(_) => break, // other side has closed channel
                    },
                }
            }
        });

        Self { rx, internal_tx }
    }

    pub async fn wait(&mut self) -> Option<()> {
        self.rx.recv().await
    }

    pub async fn ping_after(&mut self, duration: Duration) -> Result<()> {
        self.internal_tx.send(duration).await?;
        Ok(())
    }
}
