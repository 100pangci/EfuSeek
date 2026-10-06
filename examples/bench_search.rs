//! Manual, non-CI benchmark. Synthetic SQLite entries only; never generates EFU.
use anyhow::{Context, Result, bail};
use efuseek::core::{
    index::{Index, IndexInfo, SourceStamp},
    query::Query,
    sort::Sort,
};
use rusqlite::{Connection, Error, ErrorCode, params};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn generate(path: &Path, count: u64) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite {}", path.display());
    }
    let mut db = Connection::open(path)?;
    // Mirrors schema 2. No production source metadata is fabricated/reused.
    db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
        CREATE TABLE entries(id INTEGER PRIMARY KEY,name TEXT NOT NULL,path TEXT NOT NULL,parent TEXT NOT NULL,extension TEXT NOT NULL,size TEXT,modified TEXT,attributes INTEGER,is_dir INTEGER NOT NULL,name_key TEXT NOT NULL,path_key TEXT NOT NULL,size_key TEXT,modified_key INTEGER);
        CREATE VIRTUAL TABLE search USING fts5(name_key,path_key,content='entries',content_rowid='id',tokenize='trigram');
        CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let tx = db.transaction()?;
    {
        let mut insert =
            tx.prepare("INSERT INTO entries VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)")?;
        for n in 0..count {
            let dir = n % 20 == 0;
            let extension = if dir {
                ""
            } else if n % 3 == 0 {
                "mkv"
            } else {
                "txt"
            };
            let marker = ["alpha", "中文", "の", "beta"][n as usize % 4];
            let name = format!("{marker}{n:07}.{extension}");
            let parent = format!("/data/Anime/batch{:03}", n % 100);
            let path = format!("{parent}/{name}");
            let size = (!dir).then(|| (n * 1024).to_string());
            insert.execute(params![
                n + 1,
                name,
                path,
                parent,
                extension,
                size,
                "116444736000000000",
                if dir { 16 } else { 32 },
                dir,
                name.to_lowercase(),
                path.to_lowercase(),
                (!dir).then(|| format!("{:020}", n * 1024)),
                n as i64
            ])?;
        }
    }
    tx.execute_batch("INSERT INTO search(search) VALUES('rebuild'); INSERT INTO search(search) VALUES('optimize');
        CREATE INDEX entries_name ON entries(name_key,id);
        CREATE INDEX entries_name_desc ON entries(name_key DESC,id ASC);")?;
    for field in ["size_key", "modified_key"] {
        for direction in ["ASC", "DESC"] {
            tx.execute_batch(&format!("CREATE INDEX entries_{field}_{direction} ON entries(({field} IS NULL),{field} {direction},id)"))?;
        }
    }
    let info = IndexInfo {
        source: SourceStamp {
            path: "synthetic-not-an-efu".into(),
            size: 0,
            modified_ns: "0".into(),
        },
        count,
        skipped: 0,
        built_at: 0,
    };
    tx.execute(
        "INSERT INTO metadata VALUES('info',?1)",
        [toml::to_string(&info)?],
    )?;
    tx.pragma_update(None, "user_version", 2)?;
    tx.commit()?;
    Ok(())
}

fn rss_kib() -> String {
    fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .unwrap_or("RSS unavailable")
        .to_owned()
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let path = match args.as_slice() {
        [flag, count, path] if flag == "--generate" => {
            generate(Path::new(path), count.to_str().context("count")?.parse()?)?;
            path
        }
        [path] => path,
        _ => bail!("usage: bench_search DATABASE | --generate COUNT NEW_DATABASE"),
    };
    let index = Index::open(Path::new(path))?;
    println!(
        "entries={} database_bytes={} {}",
        index.info.count,
        fs::metadata(path)?.len(),
        rss_kib()
    );
    println!(
        "query,count,prepare_ms,matches_ms,first_page_read_ms,first_result_ms,deep_page_ms,temp_logical_bytes,rss"
    );
    let mut broad_time = Duration::ZERO;
    for text in ["a", "1", "中", "の", "01", "file:a", "ext:mkv", "path:a"] {
        let stats = index.prepare_search_measured(&Query::parse(text), Sort::default())?;
        if text == "a" {
            broad_time = stats.total;
        }
        let start = Instant::now();
        index.search_page(0, 128)?;
        let first = start.elapsed();
        let start = Instant::now();
        index.search_page(
            stats.count.saturating_sub(128).min(u64::from(u32::MAX)) as u32,
            128,
        )?;
        println!(
            "{text},{},{:.3},{:.3},{:.3},{:.3},{:.3},{},{}",
            stats.count,
            stats.total.as_secs_f64() * 1000.,
            stats.materialize.as_secs_f64() * 1000.,
            first.as_secs_f64() * 1000.,
            (stats.total + first).as_secs_f64() * 1000.,
            start.elapsed().as_secs_f64() * 1000.,
            stats.temp_bytes,
            rss_kib()
        );
    }
    // Cancellation-to-return latency, not total query duration. Report races as
    // completed, never pretend a query that finished before cancellation stopped.
    let latest = Arc::new(AtomicU64::new(1));
    let stop = Arc::new(AtomicBool::new(false));
    for (phase, delay) in [
        ("early", Duration::from_millis(10)),
        ("middle", broad_time / 2),
        ("late", broad_time.mul_f64(0.9)),
    ] {
        let mut latencies = Vec::new();
        for _ in 0..20 {
            latest.store(1, Ordering::Relaxed);
            index.set_cancellation(latest.clone(), 1, stop.clone());
            let latest = latest.clone();
            let result = thread::scope(|scope| {
                let cancel = scope.spawn(move || {
                    thread::sleep(delay);
                    let start = Instant::now();
                    latest.store(2, Ordering::Relaxed);
                    start
                });
                let result = index.prepare_search(&Query::parse("a"), Sort::default());
                let returned = Instant::now();
                let cancelled = cancel.join().expect("cancel thread");
                (result, returned.checked_duration_since(cancelled))
            });
            if let (Err(error), latency) = result {
                if !matches!(error.downcast_ref::<Error>(), Some(Error::SqliteFailure(code, _)) if code.code == ErrorCode::OperationInterrupted)
                {
                    return Err(error);
                }
                if let Some(latency) = latency {
                    latencies.push(latency.as_secs_f64() * 1000.);
                }
            }
        }
        if !latencies.is_empty() {
            println!(
                "cancel_phase={phase} delay_ms={:.3} samples={} mean_ms={:.3} max_ms={:.3}",
                delay.as_secs_f64() * 1000.,
                latencies.len(),
                latencies.iter().sum::<f64>() / latencies.len() as f64,
                latencies.iter().copied().fold(0., f64::max)
            );
        } else {
            println!("cancel_phase={phase}: no samples, queries completed before cancel");
        }
    }
    // Linux SQLite TEMP files are opened delete-on-close/unlinked. Show backing
    // file observations separately from logical TEMP pages and process RSS.
    for entry in fs::read_dir("/proc/self/fd")? {
        if let Ok(target) = fs::read_link(entry?.path())
            && target.to_string_lossy().contains("etilqs")
        {
            println!("sqlite_temp_fd={}", target.display());
        }
    }
    Ok(())
}
