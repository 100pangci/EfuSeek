use crate::{
    config::Config,
    core::{
        entry::Entry,
        index::{self, Index, IndexInfo, SourceStamp},
        query::Query,
        sort::Sort,
    },
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

pub enum Event {
    Status {
        message: String,
        info: Option<IndexInfo>,
        changed: bool,
    },
    Results {
        generation: u64,
        entries: Vec<Entry>,
    },
    SearchError {
        generation: u64,
        message: String,
    },
    Page {
        generation: u64,
        offset: u32,
        entries: Vec<Entry>,
    },
}
pub struct PageRequest {
    pub generation: u64,
    pub offset: u32,
    pub limit: u32,
    pub sort: Sort,
}
pub struct SearchRequest {
    pub generation: u64,
    pub text: String,
    pub sort: Sort,
}
pub struct Workers {
    pub search: Sender<SearchRequest>,
    pub events: Receiver<Event>,
    pub pages: Sender<PageRequest>,
    pub latest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

/// Only metadata is read while the source is changing. Errors break consecutiveness.
#[derive(Default)]
struct Stability {
    candidate: Option<SourceStamp>,
}
impl Stability {
    fn observe(&mut self, stamp: SourceStamp) -> bool {
        let stable = self.candidate.as_ref() == Some(&stamp);
        self.candidate = Some(stamp);
        stable
    }
    fn reset(&mut self) {
        self.candidate = None;
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Workers {
    pub fn start(config: Config, db: PathBuf) -> Self {
        let (send, events) = mpsc::channel();
        let (search, requests) = mpsc::channel::<SearchRequest>();
        let (pages, page_requests) = mpsc::channel::<PageRequest>();
        let stop = Arc::new(AtomicBool::new(false));
        let revision = Arc::new(AtomicU64::new(0));
        let latest = Arc::new(AtomicU64::new(0));
        let repair = Arc::new(AtomicBool::new(false));
        if config.efu_path.as_os_str().is_empty() {
            let _ = send.send(Event::Status {
                message: "尚未选择 EFU，请点击右上角设置。".into(),
                info: None,
                changed: false,
            });
            return Self {
                search,
                events,
                pages,
                latest,
                stop,
            };
        }
        {
            let (send, stop, revision, repair, db, config) = (
                send.clone(),
                stop.clone(),
                revision.clone(),
                repair.clone(),
                db.clone(),
                config.clone(),
            );
            thread::spawn(move || {
                // Adopt an existing MVP cache only when its metadata matches this source.
                let legacy = db.parent().map(|directory| directory.join("index.sqlite"));
                if !db.exists()
                    && let Some(legacy) = legacy
                    && Index::open(&legacy)
                        .is_ok_and(|index| index.info.source.path == config.efu_path)
                    && let Err(error) = std::fs::rename(&legacy, &db)
                {
                    log::warn!("旧缓存迁移失败，将重新构建：{error}");
                }
                let mut upgrade = false;
                let mut current = match Index::open(&db) {
                    Ok(index) => {
                        upgrade = index.needs_upgrade();
                        let info = index.info;
                        let _ = send.send(Event::Status {
                            message: "缓存已就绪，正在检查 EFU…".into(),
                            info: Some(info.clone()),
                            changed: true,
                        });
                        Some(info)
                    }
                    Err(error) => {
                        log::info!("缓存不可用，将尝试构建: {error:#}");
                        None
                    }
                };
                let mut stability = Stability::default();
                while !stop.load(Ordering::Relaxed) {
                    let mut waiting = false;
                    let result = SourceStamp::read(&config.efu_path).and_then(|stamp| {
                        if current.as_ref().is_some_and(|i| i.source == stamp)
                            && !repair.load(Ordering::Relaxed)
                            && !upgrade
                        {
                            stability.reset();
                            return Ok(None);
                        }
                        if !stability.observe(stamp.clone()) {
                            waiting = true;
                            return Ok(None);
                        }
                        stability.reset();
                        let _ = send.send(Event::Status {
                            message: "正在更新索引…".into(),
                            info: current.clone(),
                            changed: false,
                        });
                        index::rebuild_stamped(&config.efu_path, &db, Some(&stamp)).map(Some)
                    });
                    let (message, changed) = match result {
                        Ok(Some(info)) => {
                            let skipped = info.skipped;
                            current = Some(info);
                            repair.store(false, Ordering::Relaxed);
                            upgrade = false;
                            revision.fetch_add(1, Ordering::Release);
                            (
                                if skipped > 0 {
                                    format!("索引最新（跳过 {skipped} 条异常行）")
                                } else {
                                    "索引最新".into()
                                },
                                true,
                            )
                        }
                        Ok(None) if waiting => ("检测到索引更新，等待文件写入完成…".into(), false),
                        Ok(None) => ("索引最新".into(), false),
                        Err(error) => {
                            stability.reset();
                            log::warn!("更新索引失败: {error:#}");
                            (format!("索引更新失败：{error:#}（保留缓存）"), false)
                        }
                    };
                    if send
                        .send(Event::Status {
                            message,
                            info: current.clone(),
                            changed,
                        })
                        .is_err()
                    {
                        break;
                    }
                    for _ in 0..config.poll_seconds * 10 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            });
        }
        {
            let stop = stop.clone();
            let latest = latest.clone();
            let db = db.clone();
            let revision = revision.clone();
            let send = send.clone();
            let repair = repair.clone();
            thread::spawn(move || {
                let mut cached: Option<Index> = None;
                let mut seen_revision = u64::MAX;
                while !stop.load(Ordering::Relaxed) {
                    let Ok(mut request) = requests.recv_timeout(Duration::from_millis(100)) else {
                        continue;
                    };
                    while let Ok(newer) = requests.try_recv() {
                        request = newer;
                    }
                    if latest.load(Ordering::Relaxed) != request.generation {
                        continue;
                    }
                    let now = revision.load(Ordering::Acquire);
                    let result = (|| -> anyhow::Result<Vec<Entry>> {
                        if cached.is_none() || now != seen_revision {
                            cached = Some(Index::open(&db)?);
                            seen_revision = now;
                        }
                        match cached.as_ref() {
                            Some(index) => {
                                index.set_cancellation(
                                    latest.clone(),
                                    request.generation,
                                    stop.clone(),
                                );
                                index.search_sorted(
                                    &Query::parse(&request.text),
                                    config.result_limit,
                                    request.sort,
                                )
                            }
                            None => anyhow::bail!("暂无可用索引"),
                        }
                    })();
                    if latest.load(Ordering::Relaxed) != request.generation
                        || stop.load(Ordering::Relaxed)
                    {
                        continue;
                    }
                    let event = match result {
                        Ok(entries) => Event::Results {
                            generation: request.generation,
                            entries,
                        },
                        Err(error) => {
                            cached = None;
                            repair.store(true, Ordering::Relaxed);
                            Event::SearchError {
                                generation: request.generation,
                                message: format!("搜索暂不可用：{error:#}"),
                            }
                        }
                    };
                    if send.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        {
            let stop = stop.clone();
            let latest = latest.clone();
            thread::spawn(move || {
                let mut cached: Option<Index> = None;
                let mut seen_revision = u64::MAX;
                while !stop.load(Ordering::Relaxed) {
                    let Ok(request) = page_requests.recv_timeout(Duration::from_millis(100)) else {
                        continue;
                    };
                    if latest.load(Ordering::Relaxed) != request.generation {
                        continue;
                    }
                    let now = revision.load(Ordering::Acquire);
                    let result = (|| -> anyhow::Result<Vec<Entry>> {
                        if cached.is_none() || now != seen_revision {
                            cached = Some(Index::open(&db)?);
                            seen_revision = now;
                        }
                        match cached.as_ref() {
                            Some(index) => {
                                index.set_cancellation(
                                    latest.clone(),
                                    request.generation,
                                    stop.clone(),
                                );
                                index.browse_sorted(request.offset, request.limit, request.sort)
                            }
                            None => anyhow::bail!("暂无可用索引"),
                        }
                    })();
                    if latest.load(Ordering::Relaxed) != request.generation {
                        continue;
                    }
                    let event = match result {
                        Ok(entries) => Event::Page {
                            generation: request.generation,
                            offset: request.offset,
                            entries,
                        },
                        Err(error) => {
                            cached = None;
                            repair.store(true, Ordering::Relaxed);
                            Event::SearchError {
                                generation: request.generation,
                                message: format!("列表加载失败：{error:#}"),
                            }
                        }
                    };
                    if send.send(event).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            search,
            events,
            pages,
            latest,
            stop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consecutive_stamps_only_and_errors_reset() {
        let mut gate = Stability::default();
        let mut stamp = SourceStamp {
            path: "/test.efu".into(),
            size: 1,
            modified_ns: "1".into(),
        };
        for size in 1..10 {
            stamp.size = size;
            assert!(!gate.observe(stamp.clone()));
        }
        assert!(gate.observe(stamp.clone()));
        gate.reset();
        assert!(!gate.observe(stamp.clone()));
        assert!(gate.observe(stamp.clone()));
        stamp.modified_ns = "2".into();
        assert!(!gate.observe(stamp));
    }
}
