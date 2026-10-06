use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathMap {
    pub from: String,
    pub to: String,
}

pub fn map_path(path: &str, maps: &[PathMap]) -> Result<PathBuf> {
    if path.contains('\0') {
        bail!("路径含无效的 NUL 字符");
    }
    let linux = path.starts_with('/') && !path.starts_with("//");
    let normalized = if linux {
        path.to_owned()
    } else {
        path.replace('\\', "/")
    };
    let found = maps
        .iter()
        .filter_map(|map| {
            let windows = !map.from.starts_with('/') || map.from.starts_with("//");
            let from = if windows {
                map.from.replace('\\', "/")
            } else {
                map.from.clone()
            };
            if from.is_empty() {
                return None;
            }
            let prefix = normalized.get(..from.len())?;
            let matches = if windows {
                prefix.eq_ignore_ascii_case(&from)
            } else {
                prefix == from
            };
            let boundary = from.ends_with('/')
                || normalized.len() == from.len()
                || normalized.as_bytes().get(from.len()) == Some(&b'/');
            (matches && boundary).then_some((map, from.len()))
        })
        .max_by_key(|(_, len)| *len);
    if let Some((map, len)) = found {
        let mut target = PathBuf::from(&map.to);
        if !target.is_absolute() || map.to.contains('\0') {
            bail!("映射目标必须为有效的 Linux 绝对路径");
        }
        for component in normalized[len..].split('/').filter(|s| !s.is_empty()) {
            if component == ".." {
                bail!("映射路径含不安全的父目录组件");
            }
            if component != "." {
                target.push(component);
            }
        }
        return Ok(target);
    }
    if normalized.starts_with('/') && !normalized.starts_with("//") {
        return Ok(PathBuf::from(path));
    }
    bail!("Windows / UNC 路径尚未配置映射")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn longest_and_unicode() {
        let maps = vec![
            PathMap {
                from: "Z:\\".into(),
                to: "/nas".into(),
            },
            PathMap {
                from: "Z:\\Anime".into(),
                to: "/anime".into(),
            },
        ];
        assert_eq!(
            map_path("z:\\Anime\\中文 日本.mkv", &maps).expect("mapped"),
            PathBuf::from("/anime/中文 日本.mkv")
        );
        assert_eq!(
            map_path("Z:\\AnimeOther\\x", &maps).expect("mapped"),
            PathBuf::from("/nas/AnimeOther/x")
        );
        assert!(map_path("Z:\\..\\secret", &maps).is_err());
    }
    #[test]
    fn unc_and_linux() {
        let maps = vec![PathMap {
            from: "\\\\NAS\\".into(),
            to: "/nas".into(),
        }];
        assert_eq!(
            map_path("\\\\nas\\共享\\a", &maps).expect("mapped"),
            PathBuf::from("/nas/共享/a")
        );
        assert_eq!(map_path("/a/b", &[]).expect("local"), PathBuf::from("/a/b"));
        assert!(map_path("C:\\a", &[]).is_err());
    }

    #[test]
    fn linux_literal_backslash_and_invalid_target() {
        let maps = vec![PathMap {
            from: "/nas/".into(),
            to: "/local/".into(),
        }];
        assert_eq!(
            map_path("/nas/a\\b", &maps).expect("mapped"),
            PathBuf::from("/local/a\\b")
        );
        let maps = vec![PathMap {
            from: "Z:\\".into(),
            to: "relative".into(),
        }];
        assert!(map_path("Z:\\a", &maps).is_err());
        assert!(map_path("/nas/a\0b", &[]).is_err());
    }
}
