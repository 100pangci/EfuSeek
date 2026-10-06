use crate::core::{efu, entry::Entry, query::Query};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: i64 = 1;
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
}
impl Index {
    /// EFU insertion order, using row IDs rather than OFFSET for deep-page access.
    pub fn browse(&self, offset: u32, limit: u32) -> Result<Vec<Entry>> {
        let mut statement = self.connection.prepare(
            "SELECT name,path,parent,extension,size,modified,attributes,is_dir FROM entries WHERE id>?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = statement.query_map(params![offset, limit.min(1024)], |r| {
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
        })?;
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
        if version != SCHEMA {
            bail!("索引版本不兼容，需要重建");
        }
        let text: String =
            connection.query_row("SELECT value FROM metadata WHERE key='info'", [], |r| {
                r.get(0)
            })?;
        let info = toml::from_str(&text)?;
        // Also verify the search tables are present; corrupt pages are reported by search.
        connection.prepare("SELECT e.id FROM entries e JOIN search s ON s.rowid=e.id LIMIT 0")?;
        Ok(Self { connection, info })
    }
    pub fn search(&self, query: &Query, limit: usize) -> Result<Vec<Entry>> {
        let text = query.text.to_lowercase();
        let normalized = Query::parse(&text);
        let phrase = normalized.fts_phrase();
        let join = if phrase.is_some() {
            "JOIN search ON search.rowid=e.id"
        } else {
            ""
        };
        let filter = if phrase.is_some() {
            r"search MATCH ?1 AND (e.name_key LIKE ?2 ESCAPE '\' OR e.path_key LIKE ?2 ESCAPE '\')"
        } else {
            r"(e.name_key LIKE ?2 ESCAPE '\' OR e.path_key LIKE ?2 ESCAPE '\') AND ?1 IS NULL"
        };
        let sql = format!("SELECT e.name,e.path,e.parent,e.extension,e.size,e.modified,e.attributes,e.is_dir FROM entries e {join}
            WHERE {filter} ORDER BY CASE WHEN e.name_key=?3 THEN 0 WHEN substr(e.name_key,1,length(?3))=?3 THEN 1
            WHEN instr(e.name_key,?3)>0 THEN 2 ELSE 3 END, e.name_key, e.id LIMIT ?4");
        // Empty input uses an indexed alphabetical browse instead of scanning/ranking every row.
        let (sql, phrase, pattern) = if text.is_empty() {
            ("SELECT name,path,parent,extension,size,modified,attributes,is_dir FROM entries WHERE ?1 IS NULL AND ?2 IS NOT NULL AND ?3='' ORDER BY name_key,id LIMIT ?4".to_owned(), None, String::new())
        } else {
            (sql, phrase, normalized.like_pattern())
        };
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![phrase, pattern, text, limit.min(5000) as i64],
            |r| {
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
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

struct TemporaryDatabase(PathBuf);
impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn rebuild(source: &Path, destination: &Path) -> Result<IndexInfo> {
    let before = SourceStamp::read(source)?;
    let parent = destination.parent().context("缓存路径没有父目录")?;
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary =
        TemporaryDatabase(parent.join(format!("build-{}-{nonce}.sqlite", std::process::id())));
    let mut connection = Connection::open(&temporary.0)?;
    connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY;
        CREATE TABLE entries(id INTEGER PRIMARY KEY,name TEXT NOT NULL,path TEXT NOT NULL,parent TEXT NOT NULL,extension TEXT NOT NULL,size TEXT,modified TEXT,attributes INTEGER,is_dir INTEGER NOT NULL,name_key TEXT NOT NULL,path_key TEXT NOT NULL);
        CREATE VIRTUAL TABLE search USING fts5(name_key,path_key,content='entries',content_rowid='id',tokenize='trigram');
        CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let transaction = connection.transaction()?;
    let stats = {
        let mut insert = transaction.prepare_cached("INSERT INTO entries(name,path,parent,extension,size,modified,attributes,is_dir,name_key,path_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?;
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
                entry.path.to_lowercase()
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
        assert!(index.search(&Query::parse("a"), 500).is_err());
        index.set_cancellation(latest, 2, stop);
        assert_eq!(index.search(&Query::parse("file9999"), 500)?.len(), 1);
        Ok(())
    }
}
