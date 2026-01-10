use crate::geometry_provider::{Geometry, GeometryProvider};
use anyhow::Result;
use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, serde::Deserialize)]
struct HyprlandActiveWindow {
    at: [i32; 2],
    size: [i32; 2],
}

pub struct HyprlandIpc {
    socket_path: PathBuf,
}

impl GeometryProvider for HyprlandIpc {
    fn new() -> Result<Self> {
        let hyprland_instance_signature = env::var("HYPRLAND_INSTANCE_SIGNATURE")?;
        let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR")?;

        // TODO: sanitize what we read from the env variables
        let socket_path = PathBuf::from(format!(
            "{}/hypr/{}/.socket.sock",
            xdg_runtime_dir, hyprland_instance_signature
        ));

        if !socket_path.exists() {
            anyhow::bail!("Hyprland socket not found at {:?}", socket_path);
        }

        Ok(Self { socket_path })
    }

    fn get_active_window_geometry(&mut self) -> Result<Geometry> {
        let json_response = self.send_command("activewindow")?;

        let window: HyprlandActiveWindow = serde_json::from_str(&json_response)?;

        let x = window.at[0];
        let y = window.at[1];
        let width = window.size[0];
        let height = window.size[1];

        Ok((x, y, width, height))
    }
}

impl HyprlandIpc {
    fn send_command(&self, command: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket_path)?;

        stream.set_read_timeout(Duration::from_secs(1).into())?;
        stream.set_write_timeout(Duration::from_secs(1).into())?;

        let request = format!("j/{}", command);
        stream.write_all(request.as_bytes())?;

        let mut response = String::new();
        stream.read_to_string(&mut response)?;

        Ok(response)
    }
}
