use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use egui::{Color32, hex_color};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use wgpu::Backends;

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(try_from = "String")]
pub struct ColorConfig(Color32);

impl Serialize for ColorConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let [r, g, b, a] = self.0.to_array();
        let hex = format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a);
        serializer.serialize_str(&hex)
    }
}

impl TryFrom<String> for ColorConfig {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        match Color32::from_hex(&s) {
            Ok(color) => Ok(ColorConfig(color)),
            Err(err) => Err(format!("{:?}", err)),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct WindowConfig {
    pub padding: u32,
    pub corner_radius: f32,
    pub background: ColorConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum RenderBackend {
    Default,
    Vulkan,
    Gl,
    Software,
}

impl Into<Backends> for RenderBackend {
    fn into(self) -> Backends {
        match self {
            RenderBackend::Gl => Backends::GL,
            RenderBackend::Vulkan => Backends::VULKAN,
            _ => Backends::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub render_backend: RenderBackend,
    pub window: WindowConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            render_backend: RenderBackend::Default,
            window: WindowConfig {
                padding: 10,
                corner_radius: 6.0,
                background: ColorConfig(hex_color!("#20202044")),
            },
        }
    }
}

pub enum ConfigEvent {
    Updated,
}

pub struct ConfigHandle {
    config: Config,
    event_rx: UnboundedReceiver<ConfigEvent>,
}

impl ConfigHandle {
    fn get_config_dir() -> Option<PathBuf> {
        if let Ok(config_path) = env::var("XDG_CONFIG_HOME") {
            PathBuf::from(format!("{}/alttabway", config_path)).into()
        } else if let Ok(home_path) = env::var("HOME") {
            PathBuf::from(format!("{}/.config/alttabway", home_path)).into()
        } else {
            return None;
        }
    }

    fn get_existing_config() -> anyhow::Result<(Config, PathBuf)> {
        let config_dir = Self::get_config_dir().context("Config file location could not be determined (requires XDG_CONFIG_HOME or HOME env variable to be set)")?;

        let config_file = Path::new(&config_dir).join("alttabway.toml");

        let old_config_file = Path::new(&config_dir).join("config.toml");
        if old_config_file.exists() {
            let _ = fs::remove_file(old_config_file);
        }

        if !config_file.exists() {
            let _ = fs::create_dir_all(config_dir);
            let config = Config::default();
            let serialized_config = toml::to_string_pretty(&config).unwrap();

            fs::write(&config_file, serialized_config)?;

            return Ok((config, config_file));
        }

        let config_str = fs::read_to_string(&config_file)?;

        Ok((toml::from_str(&config_str)?, config_file))
    }

    pub fn new() -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let (config, config_path) = match Self::get_existing_config() {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!("Error using config file: {}", err);
                return ConfigHandle {
                    config: Config::default(),
                    event_rx,
                };
            }
        };

        tokio::spawn(async move {
            use notify::{Event, EventKind, event::ModifyKind};

            let (tx, rx) = std::sync::mpsc::channel();

            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(watcher) => watcher,
                Err(err) => {
                    tracing::warn!("Failed to watch config file! {}", err);
                    return;
                }
            };

            if watcher
                .watch(&config_path, notify::RecursiveMode::NonRecursive)
                .is_err()
            {
                return;
            };

            while let Ok(event) = rx.recv() {
                if let Ok(Event {
                    kind: EventKind::Modify(ModifyKind::Data(_)),
                    ..
                }) = event
                {
                    if event_tx.send(ConfigEvent::Updated).is_err() {
                        break;
                    }
                }
            }
        });

        Self { config, event_rx }
    }

    pub fn get(&self) -> &Config {
        &self.config
    }

    pub async fn recv(&mut self) -> Option<ConfigEvent> {
        self.event_rx.recv().await
    }
}
