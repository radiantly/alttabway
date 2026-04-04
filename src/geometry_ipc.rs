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

pub struct SwayIpc {
    socket_path: PathBuf,
}

impl SwayIpc {
    const I3_IPC_MAGIC: &[u8] = b"i3-ipc";
    const I3_IPC_HEADER_LEN: usize = 14; // 6 magic + 4 payload_len + 4 type
    const GET_TREE: u32 = 4;
}

impl GeometryProvider for SwayIpc {
    fn new() -> Result<Self> {
        let socket_path = env::var("SWAYSOCK").map(PathBuf::from)?;

        if !socket_path.exists() {
            anyhow::bail!("Sway socket not found at {:?}", socket_path);
        }

        Ok(Self { socket_path })
    }

    fn get_active_window_geometry(&mut self) -> Result<Geometry> {
        let json_response = self.send_command(SwayIpc::GET_TREE, "")?;
        let tree: serde_json::Value = serde_json::from_str(&json_response)?;

        find_focused_geometry(&tree)
            .ok_or_else(|| anyhow::anyhow!("no focused window found in sway tree"))
    }
}

impl SwayIpc {
    fn send_command(&self, msg_type: u32, payload: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket_path)?;

        stream.set_read_timeout(Duration::from_secs(1).into())?;
        stream.set_write_timeout(Duration::from_secs(1).into())?;

        let payload_bytes = payload.as_bytes();
        let mut request = Vec::with_capacity(SwayIpc::I3_IPC_HEADER_LEN + payload_bytes.len());
        request.extend_from_slice(SwayIpc::I3_IPC_MAGIC);
        request.extend_from_slice(&(payload_bytes.len() as u32).to_le_bytes());
        request.extend_from_slice(&msg_type.to_le_bytes());
        request.extend_from_slice(payload_bytes);

        stream.write_all(&request)?;

        let mut header = [0u8; SwayIpc::I3_IPC_HEADER_LEN];
        stream.read_exact(&mut header)?;

        let payload_len = u32::from_le_bytes(header[6..10].try_into()?) as usize;

        let mut response = vec![0u8; payload_len];
        stream.read_exact(&mut response)?;

        Ok(String::from_utf8(response)?)
    }
}

/// Recursively search the sway tree for the focused leaf window.
/// Recurses into children first so leaf nodes are checked before their parents.
fn find_focused_geometry(node: &serde_json::Value) -> Option<Geometry> {
    for key in &["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|v| v.as_array()) {
            for child in children {
                if let Some(geo) = find_focused_geometry(child) {
                    return Some(geo);
                }
            }
        }
    }

    if node
        .get("focused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if let Some(rect) = node.get("rect") {
            let x = rect.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let y = rect.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let w = rect.get("width").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let h = rect.get("height").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            return Some((x, y, w, h));
        }
    }

    None
}
