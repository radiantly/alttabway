use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageReader};
use ini::Ini;
use lazy_static::lazy_static;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub struct IconHelper;

lazy_static! {
    static ref ICON_DIRS: Vec<PathBuf> = {
        let mut dirs: Vec<PathBuf> = IconHelper::ICON_SYSTEM_DIRS
            .iter()
            .map(|dir| PathBuf::from(dir))
            .collect();

        if let Ok(home_dir) = env::var("HOME") {
            for user_dir in IconHelper::ICON_USER_DIRS {
                dirs.push(Path::new(&home_dir).join(user_dir));
            }
        }

        dirs
    };
    static ref DESKTOP_DIRS: Vec<PathBuf> = {
        let mut dirs: Vec<PathBuf> = IconHelper::DESKTOP_SYSTEM_DIRS
            .iter()
            .map(|dir| PathBuf::from(dir))
            .collect();

        if let Ok(home_dir) = env::var("HOME") {
            for user_dir in IconHelper::DESKTOP_USER_DIRS {
                dirs.push(Path::new(&home_dir).join(user_dir));
            }
        }

        dirs
    };
}

impl IconHelper {
    const ICON_SYSTEM_DIRS: [&str; 3] = [
        "/usr/share/icons/hicolor/256x256/apps",
        "/usr/share/icons/hicolor/48x48/apps",
        "/usr/share/pixmaps",
    ];
    const ICON_USER_DIRS: [&str; 2] = [".local/share/icons", ".icons"];

    const DESKTOP_SYSTEM_DIRS: [&str; 1] = ["/usr/share/applications"];
    const DESKTOP_USER_DIRS: [&str; 1] = [".local/share/applications"];

    fn find_icon_file(file_name: &str) -> Option<PathBuf> {
        for icon_dir in ICON_DIRS.iter() {
            let path = icon_dir.join(file_name);
            if path.exists() {
                return Some(path);
            }
        }

        None
    }

    fn get_desktop_files() -> Vec<PathBuf> {
        let mut paths = vec![];

        for desktop_dir in DESKTOP_DIRS.iter() {
            let Ok(dir_iter) = fs::read_dir(desktop_dir) else {
                continue;
            };
            for entry in dir_iter {
                let Ok(entry) = entry else { continue };

                let path = entry.path();
                if path.is_file() && path.extension() == Some(OsStr::new("desktop")) {
                    paths.push(path);
                }
            }
        }

        paths
    }

    fn get_icon_for_app_id(app_id: &str) -> Option<DynamicImage> {
        for desktop_file in Self::get_desktop_files() {
            tracing::info!("{:?}", desktop_file);
            let Ok(ini) = Ini::load_from_file(&desktop_file) else {
                continue;
            };

            let file_stem_matches = desktop_file
                .file_stem()
                .and_then(OsStr::to_str)
                .is_some_and(|stem| stem == app_id);

            let exec_matches = ini
                .section(Some("Desktop Entry"))
                .and_then(|section| section.get("Exec"))
                .is_some_and(|exec| Self::exec_matches_app_id(exec, app_id));

            if !file_stem_matches && !exec_matches {
                continue;
            }

            if let Some(icon_path) = ini
                .section(Some("Desktop Entry"))
                .and_then(|section| section.get("Icon"))
                .and_then(|icon_value| Self::resolve_icon_path(icon_value))
            {
                if let Ok(icon) = Self::read_image(icon_path) {
                    return icon.into();
                }
            }
        }

        None
    }

    fn read_image(path: PathBuf) -> anyhow::Result<DynamicImage> {
        Ok(ImageReader::open(path)?.decode()?)
    }

    fn exec_matches_app_id(exec: &str, app_id: &str) -> bool {
        let Some(token) = exec.split_whitespace().next() else {
            return false;
        };

        let token = token.trim_matches('"');
        if token == app_id {
            return true;
        }

        Path::new(token)
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|stem| stem == app_id)
    }

    fn resolve_icon_path(icon_value: &str) -> Option<PathBuf> {
        let icon_value = icon_value.trim();
        if icon_value.is_empty() {
            return None;
        }

        let icon_path = Path::new(icon_value);
        if icon_path.is_absolute() && icon_path.exists() {
            return Some(icon_path.to_path_buf());
        }

        if icon_path.extension().is_some() {
            return Self::find_icon_file(icon_value);
        }

        let file_name = format!("{}.png", icon_value);
        if let Some(path) = Self::find_icon_file(&file_name) {
            return Some(path);
        }

        None
    }
}

pub struct IconWorker {
    sender: UnboundedSender<(String, DynamicImage)>,
    receiver: UnboundedReceiver<(String, DynamicImage)>,
}

impl IconWorker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    pub fn get_icon(&mut self, app_id: impl Into<String>) {
        let app_id = app_id.into();
        let sender = self.sender.clone();
        tokio::spawn(async move {
            if let Some(icon) = IconHelper::get_icon_for_app_id(&app_id) {
                let _ = sender.send((app_id, icon));
            }
        });
    }

    pub async fn recv(&mut self) -> Option<(String, DynamicImage)> {
        self.receiver.recv().await
    }
}
