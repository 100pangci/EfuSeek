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
    let path = if parent { &entry.parent } else { &entry.path };
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
