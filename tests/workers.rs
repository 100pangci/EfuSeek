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

#[test]
fn rapid_input_clear_and_sort_ignore_stale_requests() -> Result<()> {
    use efuseek::core::sort::{Sort, SortDirection, SortField};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("test.efu");
    let db = directory.path().join("cache.sqlite");
    fs::write(
        &source,
        "Filename\n/Anime/abc.mkv\n/Anime/ab.mkv\n/Other/a.txt\n",
    )?;
    rebuild(&source, &db)?;
    fs::remove_file(&source)?;
    let workers = Workers::start(
        Config {
            efu_path: source,
            ..Config::default()
        },
        db,
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    search(&workers, 1, "a")?;
    // Make stale-queue rejection deterministic: publish final generation first,
    // then enqueue intermediate input/sort/page requests (no timing sleeps).
    workers.latest.store(20, Ordering::Relaxed);
    for (n, text) in ["a", "ab", "abc", "ab", "a", "", "ext:mkv", "path:Anime"]
        .iter()
        .enumerate()
    {
        let generation = n as u64 + 2;
        let sort = Sort {
            field: [SortField::Name, SortField::Size, SortField::Modified][n % 3],
            direction: SortDirection::Descending,
        };
        workers.search.send(SearchRequest {
            generation,
            text: (*text).into(),
            sort,
        })?;
        workers.pages.send(PageRequest {
            generation,
            offset: 0,
            limit: 2,
            sort,
            searching: !text.is_empty(),
        })?;
    }
    let sort = Sort {
        field: SortField::Name,
        direction: SortDirection::Descending,
    };
    workers.search.send(SearchRequest {
        generation: 20,
        text: "path:Anime".into(),
        sort,
    })?;
    loop {
        match workers.events.recv_timeout(Duration::from_secs(10))? {
            Event::Results {
                generation,
                count,
                entries,
            } => {
                assert_eq!(generation, 20);
                assert_eq!(count, 2);
                assert_eq!(entries[0].name, "abc.mkv");
                break;
            }
            Event::SearchError { message, .. } => bail!("unexpected error: {message}"),
            Event::Page { .. } => bail!("stale page published"),
            Event::Status { .. } => (),
        }
    }
    workers.pages.send(PageRequest {
        generation: 20,
        offset: 1,
        limit: 1,
        sort,
        searching: true,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 20, ..}))?, Event::Page {entries, ..} if entries[0].name == "ab.mkv")
    );
    // Clear actually returns to browse, then another filter prepares a new table.
    workers.latest.store(21, Ordering::Relaxed);
    workers.pages.send(PageRequest {
        generation: 21,
        offset: 0,
        limit: 3,
        sort,
        searching: false,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 21, ..}))?, Event::Page {entries, ..} if entries.len() == 3)
    );
    assert!(matches!(
        search(&workers, 22, "ext:mkv")?,
        Event::Results { count: 2, .. }
    ));
    Ok(())
}

#[test]
fn worker_error_then_repaired_cache_needs_no_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("offline.efu");
    let db = directory.path().join("cache.sqlite");
    fs::write(&db, "corrupted cache")?;
    let workers = Workers::start(
        Config {
            efu_path: source.clone(),
            ..Config::default()
        },
        db.clone(),
    );
    workers.latest.store(1, Ordering::Relaxed);
    workers.search.send(SearchRequest {
        generation: 1,
        text: "a".into(),
        sort: Default::default(),
    })?;
    wait(&workers, |e| {
        matches!(e, Event::SearchError { generation: 1, .. })
    })?;
    // Source becomes reachable and cache is repaired atomically. The same worker
    // must reopen successfully even without a revision notification.
    fs::write(&source, "Filename\n/Anime/abc.mkv\n")?;
    rebuild(&source, &db)?;
    fs::remove_file(&source)?;
    assert!(
        matches!(search(&workers, 2, "ext:mkv")?, Event::Results {count: 1, entries, ..} if entries[0].name == "abc.mkv")
    );
    Ok(())
}

#[test]
fn index_revision_keeps_prepared_pages_on_old_inode() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("test.efu");
    let db = directory.path().join("cache.sqlite");
    fs::write(&source, "Filename\n/old/a.mkv\n/old/b.mkv\n")?;
    rebuild(&source, &db)?;
    let workers = Workers::start(
        Config {
            efu_path: source.clone(),
            poll_seconds: 1,
            ..Config::default()
        },
        db,
    );
    wait(&workers, |e| {
        matches!(e, Event::Status { changed: true, .. })
    })?;
    assert!(matches!(
        search(&workers, 1, "ext:mkv")?,
        Event::Results { count: 2, .. }
    ));
    fs::write(&source, "Filename\n/new/c.mkv\n")?;
    wait(
        &workers,
        |e| matches!(e, Event::Status {changed: true, info: Some(i), ..} if i.count == 1),
    )?;
    workers.pages.send(PageRequest {
        generation: 1,
        offset: 1,
        limit: 1,
        sort: Default::default(),
        searching: true,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 1, ..}))?, Event::Page {entries, ..} if entries[0].path == "/old/b.mkv")
    );
    assert!(
        matches!(search(&workers, 2, "ext:mkv")?, Event::Results {count: 1, entries, ..} if entries[0].path == "/new/c.mkv")
    );
    // Browse pages also belong to a snapshot, even though no matches table is
    // needed. An updater notification must not swap their inode mid-generation.
    workers.latest.store(3, Ordering::Relaxed);
    workers.pages.send(PageRequest {
        generation: 3,
        offset: 0,
        limit: 2,
        sort: Default::default(),
        searching: false,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 3, ..}))?, Event::Page {entries, ..} if entries[0].path == "/new/c.mkv")
    );
    fs::write(&source, "Filename\n/newer/d.mkv\n/newer/e.mkv\n")?;
    wait(
        &workers,
        |e| matches!(e, Event::Status {changed: true, info: Some(i), ..} if i.count == 2),
    )?;
    workers.pages.send(PageRequest {
        generation: 3,
        offset: 0,
        limit: 2,
        sort: Default::default(),
        searching: false,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 3, ..}))?, Event::Page {entries, ..} if entries.len() == 1 && entries[0].path == "/new/c.mkv")
    );
    workers.latest.store(4, Ordering::Relaxed);
    workers.pages.send(PageRequest {
        generation: 4,
        offset: 0,
        limit: 2,
        sort: Default::default(),
        searching: false,
    })?;
    assert!(
        matches!(wait(&workers, |e| matches!(e, Event::Page {generation: 4, ..}))?, Event::Page {entries, ..} if entries.len() == 2 && entries[0].path == "/newer/d.mkv")
    );
    Ok(())
}
