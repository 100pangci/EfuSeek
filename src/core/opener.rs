use crate::core::{
    entry::Entry,
    path_map::{PathMap, map_path},
};
use gio::prelude::*;

/// GIO performs potentially slow filesystem access asynchronously, including SMB mounts.
pub fn open(
    entry: &Entry,
    parent: bool,
    maps: &[PathMap],
    done: impl FnOnce(Result<(), String>) + 'static,
) {
    let path = opening_path(entry, parent);
    let target = match map_path(path, maps) {
        Ok(target) => target,
        Err(error) => {
            done(Err(error.to_string()));
            return;
        }
    };
    let file = gio::File::for_path(target);
    let uri = file.uri();
    file.query_info_async(
        "standard::type",
        gio::FileQueryInfoFlags::NONE,
        gio::glib::Priority::DEFAULT,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(error) = result {
                log::warn!("打开目标失败: {error}");
                done(Err("目标文件当前不可访问。".into()));
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

fn opening_path(entry: &Entry, parent: bool) -> &str {
    // A root directory has no EFU parent; opening itself is the useful fallback.
    if parent && !(entry.is_dir && entry.parent.is_empty()) {
        &entry.parent
    } else {
        &entry.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn containing_directory_and_roots() {
        let file = Entry::new("/nas/anime/a.mkv".into(), None, None, None);
        assert_eq!(opening_path(&file, true), "/nas/anime/");
        let folder = Entry::new("/nas/anime/".into(), None, None, None);
        assert_eq!(opening_path(&folder, true), "/nas/");
        for path in ["/", "Z:\\"] {
            let root = Entry::new(path.into(), None, None, None);
            assert_eq!(opening_path(&root, true), path);
        }
    }
}
