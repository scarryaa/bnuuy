use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub struct Colors {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub font_size: f32,
    pub shell: Vec<String>,
    pub colors: Colors,
    pub background_opacity: f32,
    #[cfg(target_os = "macos")]
    pub macos_transparent_titlebar: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 15.0,
            shell: vec!["bash".into(), "-i".into()],
            colors: Colors {
                foreground: (0xC0, 0xC0, 0xC0),
                background: (0x00, 0x00, 0x00),
            },
            background_opacity: 1.0,
            #[cfg(target_os = "macos")]
            macos_transparent_titlebar: false,
        }
    }
}

impl Config {
    /// Load config from a file, or create a default
    pub fn load() -> Result<Self, config::ConfigError> {
        let config_path = if let Some(proj_dirs) = ProjectDirs::from("lt", "scar", "bnuuy") {
            let mut path = proj_dirs.config_dir().to_path_buf();
            std::fs::create_dir_all(&path).ok();

            path.push("config.toml");
            path
        } else {
            // Fallback if home dir not found
            PathBuf::from("config.toml")
        };

        let s = config::Config::builder()
            .add_source(config::Config::try_from(&Self::default())?)
            .add_source(config::File::from(config_path).required(false))
            .build()?;

        s.try_deserialize()
    }
}
