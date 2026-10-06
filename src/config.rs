use crate::core::path_map::PathMap;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub efu_path: PathBuf,
    pub result_limit: usize,
    pub poll_seconds: u64,
    pub path_map: Vec<PathMap>,
    pub cache_dir: PathBuf,
    pub debounce_ms: u64,
    pub window_width: i32,
    pub window_height: i32,
    pub browse_page_size: u32,
    pub browse_cache_pages: usize,
}

// Separate required-field type avoids recursive serde(default) during default loading.
#[derive(Deserialize)]
struct BuiltinDefaults {
    efu_path: PathBuf,
    path_map: Vec<PathMap>,
    cache_dir: PathBuf,
    result_limit: usize,
    poll_seconds: u64,
    debounce_ms: u64,
    window_width: i32,
    window_height: i32,
    browse_page_size: u32,
    browse_cache_pages: usize,
}
pub const DEFAULT_CONFIG: &str = include_str!("../config.default.toml");
impl Default for Config {
    fn default() -> Self {
        static DEFAULTS: std::sync::LazyLock<BuiltinDefaults> = std::sync::LazyLock::new(|| {
            toml::from_str(DEFAULT_CONFIG)
                .expect("bundled config.default.toml must be valid; covered by tests")
        });
        let defaults = &*DEFAULTS;
        Self {
            efu_path: defaults.efu_path.clone(),
            path_map: defaults.path_map.clone(),
            cache_dir: defaults.cache_dir.clone(),
            result_limit: defaults.result_limit,
            poll_seconds: defaults.poll_seconds,
            debounce_ms: defaults.debounce_ms,
            window_width: defaults.window_width,
            window_height: defaults.window_height,
            browse_page_size: defaults.browse_page_size,
            browse_cache_pages: defaults.browse_cache_pages,
        }
    }
}
fn xdg(variable: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(path) = env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(path.join("efuseek"));
    }
    Ok(PathBuf::from(env::var_os("HOME").context("HOME 未设置")?)
        .join(fallback)
        .join("efuseek"))
}
impl Config {
    pub fn config_path() -> Result<PathBuf> {
        Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("config.toml"))
    }

    pub fn database_path(&self) -> Result<PathBuf> {
        let directory = if self.cache_dir.as_os_str().is_empty() {
            Self::cache_directory()?
        } else {
            self.cache_dir.clone()
        };
        use std::os::unix::ffi::OsStrExt;
        let hash = self
            .efu_path
            .as_os_str()
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        Ok(directory.join(format!("index-{hash:016x}.sqlite")))
    }

    pub fn save_efu_path(efu_path: PathBuf) -> Result<Self> {
        Self::save_efu_path_to(&Self::config_path()?, efu_path)
    }

    pub fn save_efu_path_to(path: &std::path::Path, efu_path: PathBuf) -> Result<Self> {
        Self::update_configuration(path, efu_path, None)
    }

    pub fn save_settings(efu_path: PathBuf, maps: Vec<PathMap>) -> Result<Self> {
        Self::save_settings_to(&Self::config_path()?, efu_path, maps)
    }

    pub fn save_settings_to(
        path: &std::path::Path,
        efu_path: PathBuf,
        maps: Vec<PathMap>,
    ) -> Result<Self> {
        Self::update_configuration(path, efu_path, Some(maps))
    }

    fn update_configuration(
        path: &std::path::Path,
        efu_path: PathBuf,
        maps: Option<Vec<PathMap>>,
    ) -> Result<Self> {
        use std::{
            io::Write,
            time::{SystemTime, UNIX_EPOCH},
        };
        if !efu_path.is_absolute() {
            bail!("EFU 路径必须是绝对路径");
        }
        let text = efu_path.to_str().context("EFU 路径必须是 UTF-8")?;
        if text.contains('\0') {
            bail!("EFU 路径包含无效字符");
        }
        let original = fs::read_to_string(path).context("读取现有配置失败")?;
        let mut document: toml::Table = toml::from_str(&original)?;
        document.insert("efu_path".into(), toml::Value::String(text.to_owned()));
        if let Some(maps) = maps {
            for map in &maps {
                if map.from.is_empty() || map.from.contains('\0') {
                    bail!("映射来源不能为空或包含 NUL 字符");
                }
                if !PathBuf::from(&map.to).is_absolute() || map.to.contains('\0') {
                    bail!("映射目标必须是本机绝对路径");
                }
            }
            document.insert("path_map".into(), toml::Value::try_from(maps)?);
        }
        let updated = toml::to_string_pretty(&document)?;
        let config = toml::from_str::<Self>(&updated)?.validated()?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let parent = path.parent().context("配置路径没有父目录")?;
        let backup = parent.join(format!("config.toml.bak-{nonce}"));
        fs::copy(path, &backup).context("备份配置失败，未修改配置")?;
        let temporary = parent.join(format!("config.toml.tmp-{nonce}"));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.set_permissions(fs::metadata(path)?.permissions())?;
            file.write_all(updated.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result.context("保存配置失败")?;
        Ok(config)
    }
    pub fn load() -> Result<Self> {
        let directory = xdg("XDG_CONFIG_HOME", ".config")?;
        fs::create_dir_all(&directory)?;
        let path = directory.join("config.toml");
        if !path.exists() {
            let text = DEFAULT_CONFIG;
            use std::io::Write;
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => file.write_all(text.as_bytes())?,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.into()),
            }
        }
        let config: Self = toml::from_str(&fs::read_to_string(&path)?).context("配置文件无效")?;
        config.validated()
    }
    pub fn validated(mut self) -> Result<Self> {
        self.result_limit = self.result_limit.clamp(1, 5000);
        self.poll_seconds = self.poll_seconds.clamp(1, 86400);
        self.debounce_ms = self.debounce_ms.clamp(1, 2000);
        self.window_width = self.window_width.clamp(480, 7680);
        self.window_height = self.window_height.clamp(320, 4320);
        self.browse_page_size = self.browse_page_size.clamp(1, 1024);
        self.browse_cache_pages = self.browse_cache_pages.clamp(1, 64);
        if !self.efu_path.as_os_str().is_empty() && !self.efu_path.is_absolute() {
            bail!("efu_path 必须是绝对路径");
        }
        if !self.cache_dir.as_os_str().is_empty() && !self.cache_dir.is_absolute() {
            bail!("cache_dir 必须是绝对路径或空字符串");
        }
        Ok(self)
    }
    pub fn cache_directory() -> Result<PathBuf> {
        xdg("XDG_CACHE_HOME", ".cache")
    }
}
