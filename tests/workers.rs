use anyhow::{Result, bail};
use efuseek::{
    config::Config,
    core::{
        index::rebuild,
        worker::{Event, PageRequest, SearchRequest, Workers},
    },
};
use std::{
    fs,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

fn wait(workers: &Workers, predicate: impl Fn(&Event) -> bool) -> Result<Event> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(event) = workers.events.recv_timeout(Duration::from_millis(100))
            && predicate(&event)
        {
            return Ok(event);
        }
    }
    bail!("worker event timed out")
}
fn search(workers: &Workers, generation: u64, text: &str) -> Result<Event> {
    workers.latest.store(generation, Ordering::Relaxed);
    workers.search.send(SearchRequest {
        generation,
        text: text.into(),
    })?;
    wait(
        workers,
        |event| matches!(event, Event::Results { generation: g, .. } if *g == generation),
    )
}

#[test]
fn automatic_update_preserves_cache_on_failure() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let efu_path = directory.path().join("test.efu");
    let db = directory.path().join("cache.sqlite");
    fs::write(&efu_path, "Filename,Attributes\n/old_folder,16\n")?;
    let config = Config {
        efu_path: efu_path.clone(),
        poll_seconds: 1,
        ..Config::default()
    };
    let workers = Workers::start(config, db);
    wait(
        &workers,
        |e| matches!(e, Event::Status { changed: true, info: Some(i), .. } if i.count == 1),
    )?;
    assert!(
        matches!(search(&workers, 1, "old")?, Event::Results { entries, .. } if entries.len() == 1 && entries[0].is_dir)
    );
    workers.latest.store(4, Ordering::Relaxed);
    workers.pages.send(PageRequest {
        generation: 4,
        offset: 0,
        limit: 512,
    })?;
    let page = wait(&workers, |e| matches!(e, Event::Page { generation: 4, .. }))?;
    assert!(matches!(page, Event::Page { entries, .. } if entries.len() == 1 && entries[0].is_dir));
    fs::write(&efu_path, "Filename\n/new_file\n/another_file\n")?;
    wait(
        &workers,
        |e| matches!(e, Event::Status { changed: true, info: Some(i), .. } if i.count == 2),
    )?;
    assert!(
        matches!(search(&workers, 2, "new")?, Event::Results { entries, .. } if entries.len() == 1)
    );
    fs::write(&efu_path, "InvalidHeader\nfoo\n")?;
    wait(
        &workers,
        |e| matches!(e, Event::Status { message, .. } if message.starts_with("索引更新失败")),
    )?;
    assert!(
        matches!(search(&workers, 3, "new")?, Event::Results { entries, .. } if entries.len() == 1)
    );
    Ok(())
}

#[test]
fn offline_start_searches_existing_cache() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("test.efu");
    let db = directory.path().join("cache.sqlite");
    fs::write(&source, "Filename\n/offline.txt\n")?;
    rebuild(&source, &db)?;
    fs::remove_file(&source)?;
    let workers = Workers::start(
        Config {
            efu_path: source,
            poll_seconds: 1,
            ..Config::default()
        },
        db,
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    assert!(
        matches!(search(&workers, 1, "offline")?, Event::Results { entries, .. } if entries.len() == 1)
    );
    Ok(())
}

#[test]
fn unconfigured_start_does_not_build_a_database() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("cache.sqlite");
    let workers = Workers::start(Config::default(), db.clone());
    wait(
        &workers,
        |e| matches!(e, Event::Status { message, info: None, .. } if message.contains("尚未选择")),
    )?;
    assert!(!db.exists());
    Ok(())
}

#[test]
fn legacy_cache_migrates_without_accessing_source() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("test.efu");
    let legacy = directory.path().join("index.sqlite");
    let db = directory.path().join("index-new.sqlite");
    fs::write(&source, "Filename\n/offline.txt\n")?;
    rebuild(&source, &legacy)?;
    fs::remove_file(&source)?;
    let workers = Workers::start(
        Config {
            efu_path: source,
            ..Config::default()
        },
        db.clone(),
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    assert!(db.exists());
    assert!(!legacy.exists());
    assert!(
        matches!(search(&workers, 1, "offline")?, Event::Results { entries, .. } if entries.len() == 1)
    );
    Ok(())
}
