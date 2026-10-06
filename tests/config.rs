use efuseek::config::Config;

#[test]
fn example_and_defaults_roundtrip() -> anyhow::Result<()> {
    let config: Config = toml::from_str(include_str!("../config.example.toml"))?;
    assert_eq!(config.path_map[0].from, "Z:\\");
    assert_eq!(config.path_map[1].from, "\\\\NAS\\share\\");
    let default: Config = toml::from_str("")?;
    let roundtrip: Config = toml::from_str(&toml::to_string(&default)?)?;
    assert_eq!(roundtrip.efu_path, default.efu_path);
    assert_eq!(roundtrip.result_limit, 500);
    assert!(roundtrip.path_map.is_empty());
    assert!(roundtrip.efu_path.as_os_str().is_empty());
    assert!(roundtrip.cache_dir.as_os_str().is_empty());
    assert_eq!(roundtrip.debounce_ms, 75);
    assert_eq!(roundtrip.browse_page_size, 512);
    Ok(())
}

#[test]
fn portable_defaults_and_custom_parameters() -> anyhow::Result<()> {
    let defaults: Config = toml::from_str(efuseek::config::DEFAULT_CONFIG)?;
    defaults.validated()?;
    let configured: Config = toml::from_str(
        "efu_path='/mnt/share/files.efu'\ncache_dir='/var/tmp/efuseek'\ndebounce_ms=100\nwindow_width=900\nbrowse_page_size=128\nbrowse_cache_pages=3\n",
    )?;
    let configured = configured.validated()?;
    assert_eq!(configured.debounce_ms, 100);
    assert_eq!(configured.window_width, 900);
    assert_eq!(configured.browse_page_size, 128);
    assert_eq!(configured.browse_cache_pages, 3);
    assert_eq!(
        configured.database_path()?.parent(),
        Some(std::path::Path::new("/var/tmp/efuseek"))
    );
    let mut invalid = configured;
    invalid.cache_dir = "relative".into();
    assert!(invalid.validated().is_err());
    Ok(())
}

#[test]
fn save_settings_preserves_other_fields_and_backs_up() -> anyhow::Result<()> {
    use efuseek::core::path_map::PathMap;
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    let original = "efu_path='/old.efu'\nresult_limit=123\npoll_seconds=9\npath_map=[]\nfuture_option='keep'\n";
    fs::write(&path, original)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640))?;
    let map = PathMap {
        from: "X:\\".into(),
        to: "/nas/中文/".into(),
    };
    let config = Config::save_settings_to(&path, "/new.efu".into(), vec![map.clone()])?;
    assert_eq!(config.result_limit, 123);
    assert_eq!(config.poll_seconds, 9);
    assert_eq!(config.path_map, vec![map]);
    let document: toml::Table = toml::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(document["future_option"].as_str(), Some("keep"));
    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o640);
    let backups: Vec<_> = fs::read_dir(directory.path())?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.toml.bak-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(backups[0].path())?, original);
    let saved = fs::read_to_string(&path)?;
    assert!(Config::save_settings_to(&path, "relative.efu".into(), vec![]).is_err());
    assert!(
        Config::save_settings_to(
            &path,
            "/other.efu".into(),
            vec![PathMap {
                from: "".into(),
                to: "relative".into()
            }]
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(&path)?, saved);
    Ok(())
}
