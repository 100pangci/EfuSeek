use crate::core::{
    efu,
    entry::{Entry, modified_key},
    query::{self, Query, Term},
    sort::{Sort, SortDirection, SortField},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, functions::FunctionFlags, params, types::Value};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SCHEMA: i64 = 2;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceStamp {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ns: String,
}
impl SourceStamp {
    pub fn read(path: &Path) -> Result<Self> {
        let meta = fs::metadata(path).context("EFU 不可访问：检查 NAS 挂载和权限")?;
        if !meta.is_file() {
            bail!("EFU 不是普通文件");
        }
        Ok(Self {
            path: path.to_owned(),
            size: meta.len(),
            modified_ns: meta
                .modified()?
                .duration_since(UNIX_EPOCH)?
                .as_nanos()
                .to_string(),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub source: SourceStamp,
    pub count: u64,
    pub skipped: u64,
    pub built_at: i64,
}

pub struct Index {
    connection: Connection,
    pub info: IndexInfo,
    legacy: bool,
    matches_ready: Cell<bool>,
}

/// Optional diagnostics for the manual benchmark (no timing assertions in CI).
pub struct SearchPreparation {
    pub count: u64,
    pub materialize: Duration,
    pub total: Duration,
    /// Logical TEMP database size, not RSS or necessarily bytes written to disk.
    pub temp_bytes: u64,
}
impl Index {
    pub fn needs_upgrade(&self) -> bool {
        self.legacy
    }

    fn order_by(&self, sort: Sort, searching: bool, rank: usize) -> String {
        let direction = match sort.direction {
            SortDirection::Ascending => "ASC",
            SortDirection::Descending => "DESC",
        };
        let field = match sort.field {
            SortField::Default if !searching => return "e.id".into(),
            SortField::Default => {
                return format!(
                    "CASE WHEN e.name_key=?{rank} THEN 0 WHEN substr(e.name_key,1,length(?{rank}))=?{rank} THEN 1 WHEN instr(e.name_key,?{rank})>0 THEN 2 ELSE 3 END,e.name_key,e.id"
                );
            }
            SortField::Name => return format!("e.name_key {direction},e.id ASC"),
            SortField::Size if self.legacy => "efu_size(e.size,e.is_dir)",
            SortField::Modified if self.legacy => "efu_modified(e.modified)",
            SortField::Size => "e.size_key",
            SortField::Modified => "e.modified_key",
        };
        // Unknown metadata (including directory sizes) stays last in either direction.
        format!("({field} IS NULL) ASC,{field} {direction},e.id ASC")
    }

    pub fn browse_sorted(&self, offset: u32, limit: u32, sort: Sort) -> Result<Vec<Entry>> {
        if sort.field == SortField::Default {
            return self.browse(offset, limit);
        }
        let sql = format!(
            "SELECT e.name,e.path,e.parent,e.extension,e.size,e.modified,e.attributes,e.is_dir FROM entries e ORDER BY {} LIMIT ?1 OFFSET ?2",
            self.order_by(sort, false, 0)
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![limit.min(1024), offset], read_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
    /// EFU insertion order, using row IDs rather than OFFSET for deep-page access.
    pub fn browse(&self, offset: u32, limit: u32) -> Result<Vec<Entry>> {
        let mut statement = self.connection.prepare(
            "SELECT name,path,parent,extension,size,modified,attributes,is_dir FROM entries WHERE id>?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![offset, limit.min(1024)], read_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
    pub fn set_cancellation(
        &self,
        latest: std::sync::Arc<std::sync::atomic::AtomicU64>,
        generation: u64,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        self.connection.progress_handler(
            10_000,
            Some(move || {
                latest.load(std::sync::atomic::Ordering::Relaxed) != generation
                    || stop.load(std::sync::atomic::Ordering::Relaxed)
            }),
        );
    }
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != SCHEMA && version != 1 {
            bail!("索引版本不兼容，需要重建");
        }
        let text: String =
            connection.query_row("SELECT value FROM metadata WHERE key='info'", [], |r| {
                r.get(0)
            })?;
        let info = toml::from_str(&text)?;
        // Full search results live in a disk-backed temporary SQLite table, never in GTK.
        connection.pragma_update(None, "temp_store", "FILE")?;
        // Also verify the search tables are present; corrupt pages are reported by search.
        connection.prepare("SELECT e.id FROM entries e JOIN search s ON s.rowid=e.id LIMIT 0")?;
        if version == 1 {
            let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
            connection.create_scalar_function("efu_modified", 1, flags, |ctx| {
                Ok(ctx
                    .get::<Option<String>>(0)?
                    .as_deref()
                    .and_then(modified_key))
            })?;
            connection.create_scalar_function("efu_size", 2, flags, |ctx| {
                let size = ctx
                    .get::<Option<String>>(0)?
                    .and_then(|s| s.parse::<u64>().ok());
                Ok(if ctx.get::<bool>(1)? {
                    None
                } else {
                    size.map(|n| format!("{n:020}"))
                })
            })?;
        }
        Ok(Self {
            connection,
            info,
            legacy: version == 1,
            matches_ready: Cell::new(false),
        })
    }
    pub fn search(&self, query: &Query, limit: usize) -> Result<Vec<Entry>> {
        self.search_sorted(query, limit, Sort::default())
    }

    pub fn search_sorted(&self, query: &Query, limit: usize, sort: Sort) -> Result<Vec<Entry>> {
        let (from, order, mut values) = self.search_sql(query, sort);
        values.push((limit.min(5000) as i64).into());
        let sql = format!(
            "SELECT e.name,e.path,e.parent,e.extension,e.size,e.modified,e.attributes,e.is_dir {from} ORDER BY {order} LIMIT ?{}",
            values.len()
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values), read_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Materialize only ordered row IDs. The same connection/inode serves count and pages.
    pub fn prepare_search(&self, query: &Query, sort: Sort) -> Result<u64> {
        self.materialize_search(query, sort).map(|(count, _)| count)
    }

    pub fn prepare_search_measured(&self, query: &Query, sort: Sort) -> Result<SearchPreparation> {
        let start = Instant::now();
        let (count, materialize) = self.materialize_search(query, sort)?;
        let total = start.elapsed();
        let pages: u64 = self
            .connection
            .pragma_query_value(Some("temp"), "page_count", |r| r.get(0))?;
        let size: u64 = self
            .connection
            .pragma_query_value(Some("temp"), "page_size", |r| r.get(0))?;
        Ok(SearchPreparation {
            count,
            materialize,
            total,
            temp_bytes: pages * size,
        })
    }

    fn materialize_search(&self, query: &Query, sort: Sort) -> Result<(u64, Duration)> {
        // DDL can itself fail/be interrupted before dropping an old table. Never
        // expose that table, or an empty rolled-back INSERT, as a valid result.
        self.matches_ready.set(false);
        self.connection.execute_batch("DROP TABLE IF EXISTS temp.matches; CREATE TEMP TABLE matches(position INTEGER PRIMARY KEY,entry_id INTEGER NOT NULL);")?;
        let (from, order, values) = self.search_sql(query, sort);
        let start = Instant::now();
        // SQLite's implicit statement transaction rolls back ALL inserted IDs
        // on SQLITE_INTERRUPT/error; an additional transaction is unnecessary.
        self.connection.execute(
            &format!("INSERT INTO temp.matches(entry_id) SELECT e.id {from} ORDER BY {order}"),
            rusqlite::params_from_iter(values),
        )?;
        let elapsed = start.elapsed();
        self.matches_ready.set(true);
        Ok((self.connection.changes(), elapsed))
    }

    pub fn search_page(&self, offset: u32, limit: u32) -> Result<Vec<Entry>> {
        if !self.matches_ready.get() {
            bail!("搜索结果尚未准备完成，请重新搜索");
        }
        let mut statement = self.connection.prepare("SELECT e.name,e.path,e.parent,e.extension,e.size,e.modified,e.attributes,e.is_dir FROM temp.matches m JOIN entries e ON e.id=m.entry_id WHERE m.position>?1 ORDER BY m.position LIMIT ?2")?;
        let rows = statement.query_map(params![offset, limit.min(1024)], read_entry)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn search_sql(&self, query: &Query, sort: Sort) -> (String, String, Vec<Value>) {
        let mut values: Vec<Value> = Vec::new();
        let mut filters = vec!["1".to_owned()];
        let mut phrases = Vec::new();
        for term in &query.terms {
            let (text, column) = match term {
                Term::Extension(text) => {
                    values.push(text.clone().into());
                    filters.push(format!("e.is_dir=0 AND e.extension=?{}", values.len()));
                    continue;
                }
                Term::Text(text) => (text, None),
                Term::File(text) => {
                    filters.push("e.is_dir=0".into());
                    (text, Some("name_key"))
                }
                Term::Folder(text) => {
                    filters.push("e.is_dir=1".into());
                    (text, Some("name_key"))
                }
                Term::Path(text) => (text, Some("path_key")),
            };
            if let Some(phrase) = query::fts_phrase(text) {
                phrases.push(match column {
                    Some(column) => format!("{column} : {phrase}"),
                    None => phrase,
                });
            }
            values.push(query::like_pattern(text).into());
            let n = values.len();
            filters.push(match column {
                Some(column) => format!(r"e.{column} LIKE ?{n} ESCAPE '\'"),
                None => {
                    format!(r"(e.name_key LIKE ?{n} ESCAPE '\' OR e.path_key LIKE ?{n} ESCAPE '\')")
                }
            });
        }
        let join = if phrases.is_empty() {
            ""
        } else {
            values.push(phrases.join(" AND ").into());
            filters.push(format!("search MATCH ?{}", values.len()));
            "JOIN search ON search.rowid=e.id"
        };
        let order = if query.terms.is_empty() && sort.field == SortField::Default {
            "e.name_key,e.id".into()
        } else {
            if sort.field == SortField::Default {
                values.push(query.rank_text().to_owned().into());
            }
            self.order_by(sort, true, values.len())
        };
        (
            format!("FROM entries e {join} WHERE {}", filters.join(" AND ")),
            order,
            values,
        )
    }
}

fn read_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let size: Option<String> = r.get(4)?;
    Ok(Entry {
        name: r.get(0)?,
        path: r.get(1)?,
        parent: r.get(2)?,
        extension: r.get(3)?,
        size: size.and_then(|s| s.parse().ok()),
        modified: r.get(5)?,
        attributes: r.get(6)?,
        is_dir: r.get(7)?,
    })
}

struct TemporaryDatabase(PathBuf);
impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn rebuild(source: &Path, destination: &Path) -> Result<IndexInfo> {
    rebuild_stamped(source, destination, None)
}

pub fn rebuild_stamped(
    source: &Path,
    destination: &Path,
    expected: Option<&SourceStamp>,
) -> Result<IndexInfo> {
    let before = SourceStamp::read(source)?;
    if expected.is_some_and(|stamp| *stamp != before) {
        bail!("EFU 在稳定检查后发生变化；保留旧索引，下轮重试");
    }
    let parent = destination.parent().context("缓存路径没有父目录")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary =
        TemporaryDatabase(parent.join(format!("build-{}-{nonce}.sqlite", std::process::id())));
    let mut connection = Connection::open(&temporary.0)?;
    connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY;
        CREATE TABLE entries(id INTEGER PRIMARY KEY,name TEXT NOT NULL,path TEXT NOT NULL,parent TEXT NOT NULL,extension TEXT NOT NULL,size TEXT,modified TEXT,attributes INTEGER,is_dir INTEGER NOT NULL,name_key TEXT NOT NULL,path_key TEXT NOT NULL,size_key TEXT,modified_key INTEGER);
        CREATE VIRTUAL TABLE search USING fts5(name_key,path_key,content='entries',content_rowid='id',tokenize='trigram');
        CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let transaction = connection.transaction()?;
    let stats = {
        let mut insert = transaction.prepare_cached("INSERT INTO entries(name,path,parent,extension,size,modified,attributes,is_dir,name_key,path_key,size_key,modified_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
        let file = fs::File::open(source).context("无法打开 EFU")?;
        efu::parse(BufReader::with_capacity(1024 * 1024, file), |entry| {
            insert.execute(params![
                entry.name,
                entry.path,
                entry.parent,
                entry.extension,
                entry.size.map(|s| s.to_string()),
                entry.modified,
                entry.attributes,
                entry.is_dir,
                entry.name.to_lowercase(),
                entry.path.to_lowercase(),
                if entry.is_dir {
                    None
                } else {
                    entry.size.map(|n| format!("{n:020}"))
                },
                entry.modified_key()
            ])?;
            Ok(())
        })?
    };
    if stats.entries == 0 && stats.skipped > 0 {
        bail!("所有 EFU 行均无效，拒绝替换旧索引");
    }
    transaction.execute("INSERT INTO search(search) VALUES('rebuild')", [])?;
    transaction.execute("INSERT INTO search(search) VALUES('optimize')", [])?;
    transaction.execute("CREATE INDEX entries_name ON entries(name_key,id)", [])?;
    transaction.execute(
        "CREATE INDEX entries_name_desc ON entries(name_key DESC,id ASC)",
        [],
    )?;
    for field in ["size_key", "modified_key"] {
        for direction in ["ASC", "DESC"] {
            transaction.execute(&format!("CREATE INDEX entries_{field}_{direction} ON entries(({field} IS NULL),{field} {direction},id)"), [])?;
        }
    }
    let after = SourceStamp::read(source)?;
    if before != after {
        bail!("EFU 在读取期间发生变化；保留旧索引，下轮重试");
    }
    let info = IndexInfo {
        source: after,
        count: stats.entries,
        skipped: stats.skipped,
        built_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64,
    };
    transaction.execute(
        "INSERT INTO metadata VALUES('info',?1)",
        [toml::to_string(&info)?],
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA)?;
    transaction.commit()?;
    connection.execute_batch("INSERT INTO search(search) VALUES('integrity-check');")?;
    let check: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        bail!("新索引校验失败: {check}");
    }
    connection.close().map_err(|(_, error)| error)?;
    // Same filesystem rename: existing readers keep the old inode until reopened.
    fs::rename(&temporary.0, destination).context("原子替换数据库失败")?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    fn audit_index(dir: &Path) -> Result<Index> {
        let source = dir.join("audit.efu");
        let db = dir.join("index.sqlite");
        let mut data = String::from("Filename\n");
        for n in 0..10_000 {
            data.push_str(&format!("/Anime/abc{n:05}.mkv\n"));
        }
        fs::write(&source, data)?;
        rebuild(&source, &db)?;
        Index::open(&db)
    }

    #[test]
    fn interrupted_insert_rolls_back_and_next_search_recovers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let index = audit_index(dir.path())?;
        for (text, field) in ["a", "ab", "abc", "ab", "a", "", "ext:mkv", "path:Anime"]
            .into_iter()
            .flat_map(|text| {
                [SortField::Name, SortField::Size, SortField::Modified].map(|field| (text, field))
            })
        {
            let inserted = Arc::new(AtomicU64::new(0));
            let counter = inserted.clone();
            index.connection.update_hook(Some(
                move |_: rusqlite::hooks::Action, db: &str, table: &str, _: i64| {
                    if db == "temp" && table == "matches" {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                },
            ));
            let counter = inserted.clone();
            index
                .connection
                .progress_handler(100, Some(move || counter.load(Ordering::Relaxed) >= 512));
            let sort = Sort {
                field,
                direction: SortDirection::Descending,
            };
            assert!(
                index.prepare_search(&Query::parse(text), sort).is_err(),
                "{text}"
            );
            assert!(
                inserted.load(Ordering::Relaxed) >= 512,
                "cancel after actual writes"
            );
            index.connection.progress_handler(0, None::<fn() -> bool>);
            index
                .connection
                .update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>);
            let count: u64 =
                index
                    .connection
                    .query_row("SELECT count(*) FROM temp.matches", [], |r| r.get(0))?;
            assert_eq!(count, 0, "SQLite statement rollback, not partial IDs");
            assert!(
                index.search_page(0, 1).is_err(),
                "failed result is not valid"
            );
            assert_eq!(index.prepare_search(&Query::parse(text), sort)?, 10_000);
            assert_eq!(index.search_page(9999, 1)?.len(), 1);
        }
        Ok(())
    }

    #[test]
    fn temp_creation_failure_invalidates_old_results_and_recovers() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let index = audit_index(dir.path())?;
        assert_eq!(
            index.prepare_search(&Query::parse("a"), Sort::default())?,
            10_000
        );
        index.connection.pragma_update(None, "query_only", true)?;
        assert!(
            index
                .prepare_search(&Query::parse("missing"), Sort::default())
                .is_err()
        );
        assert!(index.search_page(0, 1).is_err());
        assert_eq!(
            index
                .connection
                .query_row("SELECT count(*) FROM temp.matches", [], |r| r
                    .get::<_, u64>(0))?,
            10_000,
            "failed DROP leaves old IDs, but they are inaccessible"
        );
        index.connection.pragma_update(None, "query_only", false)?;
        assert_eq!(
            index.prepare_search(&Query::parse("abc00001"), Sort::default())?,
            1
        );
        assert_eq!(index.search_page(0, 1)?[0].name, "abc00001.mkv");
        // Verify ID-only schema and indexed position range (no deep OFFSET).
        let mut columns = index
            .connection
            .prepare("PRAGMA temp.table_info(matches)")?;
        let columns = columns
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(columns, ["position", "entry_id"]);
        let plan: String = index.connection.query_row("EXPLAIN QUERY PLAN SELECT entry_id FROM temp.matches WHERE position>9000 ORDER BY position LIMIT 128", [], |r| r.get(3))?;
        assert!(plan.contains("INTEGER PRIMARY KEY"), "{plan}");
        Ok(())
    }

    #[test]
    fn generation_change_during_insert_interrupts_and_recovers() -> Result<()> {
        use std::sync::atomic::AtomicBool;
        let dir = tempfile::tempdir()?;
        let index = audit_index(dir.path())?;
        let latest = Arc::new(AtomicU64::new(1));
        let stop = Arc::new(AtomicBool::new(false));
        let next = latest.clone();
        index.connection.update_hook(Some(
            move |_: rusqlite::hooks::Action, db: &str, table: &str, id: i64| {
                if db == "temp" && table == "matches" && id == 512 {
                    next.store(2, Ordering::Relaxed);
                }
            },
        ));
        index.set_cancellation(latest.clone(), 1, stop.clone());
        assert!(
            index
                .prepare_search(&Query::parse("a"), Sort::default())
                .is_err()
        );
        assert_eq!(latest.load(Ordering::Relaxed), 2);
        index
            .connection
            .update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>);
        index.set_cancellation(latest, 2, stop);
        assert!(index.search_page(0, 1).is_err());
        assert_eq!(
            index.prepare_search(&Query::parse("abc00001"), Sort::default())?,
            1
        );
        Ok(())
    }

    #[test]
    fn temp_full_rolls_back_and_recovers_without_reopening() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let index = audit_index(dir.path())?;
        index
            .connection
            .pragma_update(Some("temp"), "max_page_count", 2)?;
        let error = index
            .prepare_search(&Query::parse("a"), Sort::default())
            .expect_err("TEMP quota must fail");
        assert!(
            matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DiskFull)
        );
        assert!(!index.matches_ready.get());
        assert_eq!(
            index
                .connection
                .query_row("SELECT count(*) FROM temp.matches", [], |r| r
                    .get::<_, u64>(0))?,
            0
        );
        index
            .connection
            .pragma_update(Some("temp"), "max_page_count", 100_000)?;
        assert_eq!(
            index.prepare_search(&Query::parse("a"), Sort::default())?,
            10_000
        );
        assert_eq!(index.search_page(9999, 1)?.len(), 1);
        Ok(())
    }

    #[test]
    fn disk_temp_is_unlinked_even_after_abrupt_exit() -> Result<()> {
        const CHILD: &str = "EFUSEEK_TEMP_AUDIT_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let index = audit_index(&root)?;
            index
                .connection
                .pragma_update(Some("temp"), "cache_size", 1)?;
            index.prepare_search(&Query::parse("a"), Sort::default())?;
            let mut found = false;
            for entry in fs::read_dir("/proc/self/fd")? {
                if let Ok(target) = fs::read_link(entry?.path())
                    && target.to_string_lossy().contains("etilqs")
                {
                    assert!(target.to_string_lossy().ends_with(" (deleted)"));
                    assert!(target.starts_with(root.join("sqlite-temp")));
                    found = true;
                }
            }
            assert!(found, "force FILE backing rather than only memory cache");
            fs::write(root.join("ready"), "ready")?;
            loop {
                std::thread::park();
            }
        }
        let dir = tempfile::tempdir()?;
        let temp = dir.path().join("sqlite-temp");
        fs::create_dir(&temp)?;
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "core::index::tests::disk_temp_is_unlinked_even_after_abrupt_exit",
                "--nocapture",
            ])
            .env(CHILD, dir.path())
            .env("SQLITE_TMPDIR", &temp)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !dir.path().join("ready").exists() && Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready = dir.path().join("ready").exists();
        let _ = child.kill(); // SIGKILL only our isolated test child, no destructors.
        child.wait()?;
        assert!(ready, "child did not verify TEMP backing");
        assert_eq!(fs::read_dir(temp)?.count(), 0, "no persistent TEMP garbage");
        Ok(())
    }
    #[test]
    fn query_filters_literals_and_ranking() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("test.efu");
        let db = dir.path().join("index.sqlite");
        fs::write(
            &source,
            "Filename,Attributes\n/Galgame/erasmus.zip,32\n/Galgame/patch_%.ZIP,32\n/Galgame/Anime,16\n/Galgame/Anime.zip,16\n/日本/中文.mkv,32\n/日本/a\\b.txt,32\n/日本/a'b.txt,32\n/日本/foo:bar,32\n/Galgame/foo patch.zip,32\n",
        )?;
        rebuild(&source, &db)?;
        let index = Index::open(&db)?;
        for (query, count) in [
            ("erasmus ext:.ZIP", 1),
            ("file:patch path:galgame", 2),
            ("folder:Anime", 2),
            ("ext:zip", 3),
            ("path:日本", 4),
            ("file:中文 ext:mkv", 1),
            ("file: folder:Anime", 0),
            ("file:patch_%", 1),
            ("path:a\\b", 1),
            ("file:a'b", 1),
            ("foo:bar", 1),
            ("file:' OR 1=1 --", 0),
            ("ext:zip ext:mkv", 0),
        ] {
            assert_eq!(
                index.search(&Query::parse(query), 500)?.len(),
                count,
                "{query}"
            );
        }
        assert_eq!(
            index.search(&Query::parse("file:patch"), 500)?[0].name,
            "patch_%.ZIP"
        );
        Ok(())
    }

    #[test]
    fn sorted_pages_search_and_legacy_cache() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("test.efu");
        let db = dir.path().join("index.sqlite");
        fs::write(
            &source,
            "Filename,Size,Date Modified,Attributes\n/z/dup,18446744073709551615,116444736000000001,32\n/a/dup,9,116444736000000000,32\n/b/dir,0,bad,16\n/c/unknown,,,32\n/d/dup,9,116444736000000000,32\n",
        )?;
        rebuild(&source, &db)?;
        for legacy in [false, true] {
            if legacy {
                let connection = Connection::open(&db)?;
                connection.execute_batch("DROP INDEX entries_size_key_ASC; DROP INDEX entries_size_key_DESC; DROP INDEX entries_modified_key_ASC; DROP INDEX entries_modified_key_DESC; ALTER TABLE entries DROP COLUMN size_key; ALTER TABLE entries DROP COLUMN modified_key; PRAGMA user_version=1;")?;
            }
            let index = Index::open(&db)?;
            assert_eq!(index.needs_upgrade(), legacy);
            for field in [SortField::Name, SortField::Size, SortField::Modified] {
                for direction in [SortDirection::Ascending, SortDirection::Descending] {
                    let sort = Sort { field, direction };
                    if !legacy {
                        let sql = format!(
                            "EXPLAIN QUERY PLAN SELECT e.id FROM entries e ORDER BY {} LIMIT 2 OFFSET 2",
                            index.order_by(sort, false, 0)
                        );
                        let mut statement = index.connection.prepare(&sql)?;
                        let plan = statement
                            .query_map([], |row| row.get::<_, String>(3))?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        assert!(
                            !plan.iter().any(|line| line.contains("TEMP B-TREE")),
                            "{sort:?}: {plan:?}"
                        );
                    }
                    let rows = index.browse_sorted(0, 512, sort)?;
                    let mut pages = index.browse_sorted(0, 2, sort)?;
                    pages.extend(index.browse_sorted(2, 2, sort)?);
                    pages.extend(index.browse_sorted(4, 2, sort)?);
                    assert_eq!(rows, pages);
                    assert_eq!(rows, index.search_sorted(&Query::parse(""), 500, sort)?);
                    if field != SortField::Name {
                        assert_eq!(rows[3].name, "dir");
                        assert_eq!(rows[4].name, "unknown");
                        assert_eq!(
                            rows[if direction == SortDirection::Ascending {
                                2
                            } else {
                                0
                            }]
                            .path,
                            "/z/dup"
                        );
                    }
                    let matches = index.search_sorted(&Query::parse("file:dup"), 500, sort)?;
                    assert_eq!(matches.len(), 3);
                    assert!(
                        matches.iter().position(|e| e.path == "/a/dup")
                            < matches.iter().position(|e| e.path == "/d/dup")
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn search_cache_and_atomic_replacement() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("test.efu");
        let db = dir.path().join("index.sqlite");
        fs::write(
            &source,
            "Filename,Size,Attributes\n/a/erasmus.exe,17,32\n/a/Kiriue_no_Erasmus,0,16\n/a/erasmus/xxx.zip,9,32\n/a/erasmus,0,16\n/a/中文日本.mkv,10,32\n/a/a_%file,2,32\n",
        )?;
        rebuild(&source, &db)?;
        let old = Index::open(&db)?;
        let previous = SourceStamp::read(&source)?;
        let rows = old.search(&Query::parse("erasmus"), 500)?;
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].name, "erasmus");
        assert_eq!(rows[1].name, "erasmus.exe");
        assert_eq!(rows[2].name, "Kiriue_no_Erasmus");
        assert!(rows[0].is_dir);
        assert_eq!(old.search(&Query::parse("中文"), 500)?.len(), 1);
        assert_eq!(old.search(&Query::parse("文日本"), 500)?.len(), 1);
        assert_eq!(old.search(&Query::parse("_%"), 500)?.len(), 1);
        assert!(old.search(&Query::parse("\" OR *"), 500)?.is_empty());
        assert_eq!(old.search(&Query::parse(""), 2)?.len(), 2);
        let first = old.browse(0, 2)?;
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].path, "/a/erasmus.exe");
        assert_eq!(old.browse(5, 512)?.len(), 1);
        assert!(old.browse(6, 512)?.is_empty());
        fs::write(&source, "Filename\n/new\n")?;
        assert!(rebuild_stamped(&source, &db, Some(&previous)).is_err());
        assert_eq!(Index::open(&db)?.info.count, 6);
        rebuild(&source, &db)?;
        assert_eq!(old.search(&Query::parse("erasmus"), 500)?.len(), 4);
        assert_eq!(Index::open(&db)?.info.count, 1);
        fs::write(&source, "bad\n1\n")?;
        assert!(rebuild(&source, &db).is_err());
        assert_eq!(Index::open(&db)?.info.count, 1);
        Ok(())
    }
    #[test]
    fn corruption_and_missing_source() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("bad.sqlite");
        fs::write(&db, b"broken")?;
        assert!(Index::open(&db).is_err());
        assert!(rebuild(&dir.path().join("absent"), &db).is_err());
        assert_eq!(fs::read(db)?, b"broken");
        Ok(())
    }

    #[test]
    fn stale_search_can_be_cancelled() -> Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64},
        };
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("test.efu");
        let db = dir.path().join("index.sqlite");
        let mut data = String::from("Filename\n");
        for n in 0..10_000 {
            data.push_str(&format!("/data/file{n}.txt\n"));
        }
        fs::write(&source, data)?;
        rebuild(&source, &db)?;
        let index = Index::open(&db)?;
        let latest = Arc::new(AtomicU64::new(2));
        let stop = Arc::new(AtomicBool::new(false));
        index.set_cancellation(latest.clone(), 1, stop.clone());
        assert!(
            index
                .prepare_search(&Query::parse("file"), Sort::default())
                .is_err()
        );
        assert!(index.search(&Query::parse("a"), 500).is_err());
        assert!(
            index
                .browse_sorted(
                    9999,
                    1,
                    Sort {
                        field: SortField::Size,
                        direction: SortDirection::Descending
                    }
                )
                .is_err()
        );
        index.set_cancellation(latest, 2, stop);
        assert_eq!(
            index.prepare_search(&Query::parse("file"), Sort::default())?,
            10_000
        );
        assert_eq!(index.search_page(9999, 10)?.len(), 1);
        assert_eq!(index.search(&Query::parse("file9999"), 500)?.len(), 1);
        Ok(())
    }

    #[test]
    fn full_search_pages_are_unlimited_sorted_and_snapshot_safe() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("test.efu");
        let db = dir.path().join("index.sqlite");
        let mut data = String::from("Filename,Size,Date Modified\n");
        for n in 0..6100 {
            data.push_str(&format!(
                "/data/video{n:05}.mkv,{n},{}\n",
                116444736000000000_i64 + n
            ));
        }
        fs::write(&source, data)?;
        rebuild(&source, &db)?;
        let index = Index::open(&db)?;
        assert_eq!(
            index.prepare_search(&Query::parse("file:video ext:mkv"), Sort::default())?,
            6100
        );
        let rows = index.search_page(6000, 200)?;
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0].name, "video06000.mkv");
        assert_eq!(rows[99].name, "video06099.mkv");
        assert!(index.search_page(6100, 200)?.is_empty());
        assert_eq!(index.search_page(0, 10000)?.len(), 1024);
        for field in [SortField::Name, SortField::Size, SortField::Modified] {
            assert_eq!(
                index.prepare_search(
                    &Query::parse("ext:mkv"),
                    Sort {
                        field,
                        direction: SortDirection::Descending
                    }
                )?,
                6100
            );
            assert_eq!(index.search_page(0, 2)?[0].name, "video06099.mkv");
            assert_eq!(index.search_page(6099, 2)?[0].name, "video00000.mkv");
        }
        fs::write(&source, "Filename\n/new.txt\n")?;
        rebuild(&source, &db)?;
        assert_eq!(index.search_page(0, 2)?[0].name, "video06099.mkv");
        assert_eq!(
            index.prepare_search(&Query::parse("absent"), Sort::default())?,
            0
        );
        assert!(index.search_page(0, 512)?.is_empty());
        Ok(())
    }
}
