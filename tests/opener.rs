use efuseek::core::{entry::Entry, opener, path_map::PathMap};
use gio::prelude::*;
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[test]
fn mapped_video_parent_launches_directory_handler() -> anyhow::Result<()> {
    if let Some(root) = std::env::var_os("EFUSEEK_OPENER_TEST_ROOT") {
        let root = PathBuf::from(root);
        let folder = root.join("日本 video # %");
        let maps = vec![PathMap {
            from: "Z:\\video.mkv".into(),
            to: folder.join("video.mkv").to_string_lossy().into_owned(),
        }];
        let entry = Entry::new("Z:\\video.mkv".into(), None, None, None);
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        opener::open(&entry, true, &maps, move |value| {
            *output.borrow_mut() = Some(value)
        });
        let context = gio::glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while result.borrow().is_none() {
            context.iteration(false);
            anyhow::ensure!(Instant::now() < deadline, "GIO callback timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        result
            .borrow_mut()
            .take()
            .unwrap()
            .map_err(anyhow::Error::msg)?;
        let marker = root.join("launched-uri");
        while !marker.exists() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "directory handler was not launched"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let launched = fs::read_to_string(marker)?;
        // GDesktopAppInfo may translate a local file URI to a native filename.
        let file = if launched.starts_with("file:") {
            gio::File::for_uri(&launched)
        } else {
            gio::File::for_path(&launched)
        };
        assert_eq!(file.path(), Some(folder));
        // A missing target yields the actual filesystem error, not a silent failure.
        let result = Rc::new(RefCell::new(None));
        let output = result.clone();
        opener::open(&entry, false, &maps, move |value| {
            *output.borrow_mut() = Some(value)
        });
        while result.borrow().is_none() {
            context.iteration(false);
            anyhow::ensure!(Instant::now() < deadline, "GIO callback timed out");
        }
        assert!(
            result
                .borrow_mut()
                .take()
                .unwrap()
                .unwrap_err()
                .contains("video.mkv")
        );
        return Ok(());
    }
    // Run GIO in a child with isolated MIME settings; never modify the user's
    // defaults or open their file manager/video player during tests.
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    fs::create_dir_all(root.join("data/applications"))?;
    fs::create_dir_all(root.join("config"))?;
    fs::create_dir_all(root.join("日本 video # %"))?;
    let helper = root.join("directory-handler");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$EFUSEEK_OPENER_TEST_ROOT/launched-uri\"\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
    fs::write(
        root.join("data/applications/test-directory.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Test directory handler\nExec=\"{}\" %u\nMimeType=inode/directory;\n",
            helper.display()
        ),
    )?;
    fs::write(
        root.join("data/applications/test-wrong.desktop"),
        "[Desktop Entry]\nType=Application\nName=Wrong URI handler\nExec=/usr/bin/false %u\nMimeType=x-scheme-handler/file;\n",
    )?;
    fs::write(
        root.join("config/mimeapps.list"),
        "[Default Applications]\ninode/directory=test-directory.desktop\nx-scheme-handler/file=test-wrong.desktop\n",
    )?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "mapped_video_parent_launches_directory_handler",
            "--nocapture",
        ])
        .env("EFUSEEK_OPENER_TEST_ROOT", root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("DBUS_SESSION_BUS_ADDRESS", "")
        .env("GIO_USE_PORTALS", "0")
        .status()?;
    anyhow::ensure!(status.success(), "isolated GIO opener test failed");
    Ok(())
}
