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
        sort: Default::default(),
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
        sort: Default::default(),
        searching: false,
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
    // Simulate a genuine v0.1.0 schema rather than only a legacy cache filename.
    rusqlite::Connection::open(&db)?.execute_batch("DROP INDEX entries_size_key_ASC; DROP INDEX entries_size_key_DESC; DROP INDEX entries_modified_key_ASC; DROP INDEX entries_modified_key_DESC; ALTER TABLE entries DROP COLUMN size_key; ALTER TABLE entries DROP COLUMN modified_key; PRAGMA user_version=1;")?;
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

#[test]
fn growing_source_waits_without_replacing_old_index() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("growing.efu");
    let db = directory.path().join("index.sqlite");
    fs::write(&source, "Filename\n/old.txt\n")?;
    rebuild(&source, &db)?;
    let workers = Workers::start(
        Config {
            efu_path: source.clone(),
            poll_seconds: 1,
            ..Config::default()
        },
        db.clone(),
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    for count in 1..=3 {
        let data = format!(
            "Filename\n{}",
            (0..count)
                .map(|n| format!("/new{n}.txt\n"))
                .collect::<String>()
        );
        fs::write(&source, data)?;
        wait(
            &workers,
            |e| matches!(e, Event::Status { message, .. } if message.contains("等待文件写入完成")),
        )?;
        assert_eq!(
            efuseek::core::index::Index::open(&db)?.browse(0, 10)?[0].name,
            "old.txt"
        );
        assert!(
            matches!(search(&workers, count, "old")?, Event::Results { entries, .. } if entries.len() == 1)
        );
    }
    wait(
        &workers,
        |e| matches!(e, Event::Status { changed: true, info: Some(i), .. } if i.count == 3),
    )?;
    assert!(
        matches!(search(&workers, 4, "new")?, Event::Results { entries, .. } if entries.len() == 3)
    );
    let sort = efuseek::core::sort::Sort {
        field: efuseek::core::sort::SortField::Name,
        direction: efuseek::core::sort::SortDirection::Descending,
    };
    workers.latest.store(5, Ordering::Relaxed);
    workers.pages.send(PageRequest {
        generation: 5,
        offset: 0,
        limit: 1,
        sort,
        searching: false,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page { generation: 5, .. }))?, Event::Page { entries, .. } if entries[0].name == "new2.txt")
    );
    workers.latest.store(6, Ordering::Relaxed);
    workers.search.send(SearchRequest {
        generation: 6,
        text: "file:new ext:txt".into(),
        sort,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Results { generation: 6, .. }))?, Event::Results { entries, .. } if entries.len() == 3 && entries[0].name == "new2.txt")
    );
    Ok(())
}

#[test]
fn full_search_has_all_matches_and_deep_pages() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("test.efu");
    let db = directory.path().join("cache.sqlite");
    let mut data = String::from("Filename\n");
    for n in 0..6100 {
        data.push_str(&format!("/data/video{n:05}.mkv\n"));
    }
    fs::write(&source, data)?;
    rebuild(&source, &db)?;
    fs::remove_file(&source)?;
    let workers = Workers::start(
        Config {
            efu_path: source,
            browse_page_size: 128,
            ..Config::default()
        },
        db,
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    assert!(
        matches!(search(&workers, 1, "ext:mkv")?, Event::Results { count: 6100, entries, .. } if entries.len() == 128)
    );
    workers.pages.send(PageRequest {
        generation: 1,
        offset: 6000,
        limit: 128,
        sort: Default::default(),
        searching: true,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page { generation: 1, .. }))?, Event::Page { entries, .. } if entries.len() == 100 && entries[99].name == "video06099.mkv")
    );
    // Older page requests cannot contaminate a newer query or sort.
    assert!(
        matches!(search(&workers, 2, "video00001")?, Event::Results { count: 1, entries, .. } if entries.len() == 1)
    );
    workers.pages.send(PageRequest {
        generation: 1,
        offset: 512,
        limit: 128,
        sort: Default::default(),
        searching: true,
    })?;
    workers.pages.send(PageRequest {
        generation: 2,
        offset: 0,
        limit: 128,
        sort: Default::default(),
        searching: true,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page { generation: 2, .. }))?, Event::Page { entries, .. } if entries.len() == 1 && entries[0].name == "video00001.mkv")
    );
    Ok(())
}
