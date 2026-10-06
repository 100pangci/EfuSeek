use crate::core::{
    entry::Entry,
    path_map::{PathMap, map_path},
};
use gio::prelude::*;
use std::path::PathBuf;

/// GIO performs potentially slow filesystem access asynchronously, including SMB mounts.
pub fn open(
    entry: &Entry,
    parent: bool,
    maps: &[PathMap],
    done: impl FnOnce(Result<(), String>) + 'static,
) {
    let target = match target_path(entry, parent, maps) {
        Ok(target) => target,
        Err(error) => {
            log::warn!("打开路径映射失败: {error}");
            done(Err(error.to_string()));
            return;
        }
    };
    let description = if parent { "所在目录" } else { "目标" };
    let target_display = target.display().to_string();
    let file = gio::File::for_path(target);
    let uri = file.uri();
    file.query_info_async(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        gio::glib::Priority::DEFAULT,
        gio::Cancellable::NONE,
        move |result| {
            let info = match result {
                Ok(info) => info,
                Err(error) => {
                    let message = format!("无法访问{description} {target_display}：{error}");
                    log::warn!("{message}");
                    done(Err(message));
                    return;
                }
            };
            if parent && info.file_type() != gio::FileType::Directory {
                done(Err(format!(
                    "所在目录不是文件夹：{target_display}，请检查路径映射。"
                )));
                return;
            }
            if info.file_type() == gio::FileType::Directory {
                // Choose the directory handler explicitly; a file/URI handler must
                // never send the containing folder to a video player or other app.
                let Some(app) = gio::AppInfo::default_for_type("inode/directory", false) else {
                    done(Err(
                        "未设置文件管理器，请为 inode/directory 配置默认应用。".into()
                    ));
                    return;
                };
                app.launch_uris_async(
                    &[&uri],
                    gio::AppLaunchContext::NONE,
                    gio::Cancellable::NONE,
                    move |result| {
                        done(result.map_err(|error| {
                            log::warn!("启动文件管理器失败: {error}");
                            format!("无法启动文件管理器：{error}")
                        }));
                    },
                );
                return;
            }
            gio::AppInfo::launch_default_for_uri_async(
                &uri,
                gio::AppLaunchContext::NONE,
                gio::Cancellable::NONE,
                move |result| {
                    done(result.map_err(|error| format!("无法启动默认应用：{error}")));
                },
            );
        },
    );
}

pub fn target_path(entry: &Entry, parent: bool, maps: &[PathMap]) -> anyhow::Result<PathBuf> {
    // Map the complete target first. EFU's parent can fall outside the mapping
    // prefix (notably a mapped drive/share root or exact-file mapping).
    let target = map_path(&entry.path, maps)?;
    Ok(if parent {
        target.parent().unwrap_or(&target).to_owned()
    } else {
        target
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn containing_directory_and_roots() {
        let file = Entry::new("/nas/anime/a.mkv".into(), None, None, None);
        assert_eq!(
            target_path(&file, true, &[]).unwrap(),
            PathBuf::from("/nas/anime")
        );
        let folder = Entry::new("/nas/anime/".into(), None, None, None);
        assert_eq!(
            target_path(&folder, true, &[]).unwrap(),
            PathBuf::from("/nas")
        );
        assert_eq!(
            target_path(&Entry::new("/".into(), None, None, None), true, &[]).unwrap(),
            PathBuf::from("/")
        );
        let maps = vec![PathMap {
            from: "Z:\\video.mkv".into(),
            to: "/nas/video.mkv".into(),
        }];
        let file = Entry::new("Z:\\video.mkv".into(), None, None, None);
        assert_eq!(
            target_path(&file, true, &maps).unwrap(),
            PathBuf::from("/nas")
        );
        assert!(target_path(&file, true, &[]).is_err());
        let maps = vec![PathMap {
            from: "\\\\NAS\\share\\".into(),
            to: "/nas/share".into(),
        }];
        let folder = Entry::new("\\\\NAS\\share\\".into(), None, None, Some(16));
        assert_eq!(
            target_path(&folder, true, &maps).unwrap(),
            PathBuf::from("/nas")
        );
    }
}
