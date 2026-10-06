//! Optional real-data smoke test; does not scan disks or open any indexed targets.
use anyhow::{Context, Result};
use efuseek::core::{
    index::{Index, SourceStamp, rebuild},
    query::Query,
};
use std::{path::PathBuf, time::Instant};

fn main() -> Result<()> {
    env_logger::init();
    let mut args = std::env::args_os().skip(1);
    let source = PathBuf::from(args.next().context("usage: check_index EFU DATABASE")?);
    let db = PathBuf::from(args.next().context("缺少数据库路径")?);
    let start = Instant::now();
    let stamp = SourceStamp::read(&source)?;
    if !Index::open(&db).is_ok_and(|index| index.info.source == stamp) {
        let info = rebuild(&source, &db)?;
        println!(
            "built: {} entries, {} skipped, {:?}",
            info.count,
            info.skipped,
            start.elapsed()
        );
    } else {
        println!("cache reused: {:?}", start.elapsed());
    }
    let index = Index::open(&db)?;
    for text in ["", "erasmus", "mkv", "中文", "a", "不存在的文件_efuseek"] {
        let start = Instant::now();
        let rows = index.search(&Query::parse(text), 500)?;
        println!(
            "query {text:?}: {} results, {:?}",
            rows.len(),
            start.elapsed()
        );
    }
    Ok(())
}
