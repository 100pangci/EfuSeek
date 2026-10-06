use crate::core::entry::Entry;
use anyhow::{Context, Result, bail};
use std::io::Read;

#[derive(Debug, Default)]
pub struct ParseStats {
    pub entries: u64,
    pub skipped: u64,
}

pub fn parse(reader: impl Read, mut accept: impl FnMut(Entry) -> Result<()>) -> Result<ParseStats> {
    let mut csv = csv::ReaderBuilder::new().flexible(true).from_reader(reader);
    let headers = csv
        .headers()
        .context("无法读取 EFU 表头（必须为 UTF-8）")?
        .clone();
    let column = |name: &str| {
        headers.iter().position(|h| {
            h.trim_start_matches('\u{feff}')
                .trim()
                .eq_ignore_ascii_case(name)
        })
    };
    let Some(filename) = column("Filename") else {
        bail!("EFU 缺少 Filename 字段");
    };
    let size = column("Size");
    let modified = column("Date Modified");
    let attributes = column("Attributes");
    let mut stats = ParseStats::default();
    for record in csv.records() {
        let record = match record {
            Ok(row) => row,
            Err(error) => {
                stats.skipped += 1;
                log::warn!("跳过 EFU 行: {error}");
                continue;
            }
        };
        let get =
            |column: Option<usize>| column.and_then(|i| record.get(i)).filter(|v| !v.is_empty());
        let Some(path) = get(Some(filename)) else {
            stats.skipped += 1;
            log::warn!("跳过空 Filename 行");
            continue;
        };
        if path.contains('\0') {
            stats.skipped += 1;
            log::warn!("跳过含 NUL 的 Filename 行");
            continue;
        }
        let number = |column| -> Option<u64> {
            get(column).and_then(|s| match s.parse() {
                Ok(n) => Some(n),
                Err(_) => {
                    log::warn!("无效数字字段，按未知值处理");
                    None
                }
            })
        };
        let attrs = number(attributes).and_then(|v| u32::try_from(v).ok());
        accept(Entry::new(
            path.to_owned(),
            number(size),
            get(modified).map(str::to_owned),
            attrs,
        ))?;
        stats.entries += 1;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fields_and_paths() {
        let data = "\u{feff}Filename,Size,Date Modified,Attributes\n\"Z:\\日本\\a,b.mkv\",1024,123,32\n\\\\NAS\\中文,0,,16\n/a/b.zip,,,\n";
        let mut entries = vec![];
        let stats = parse(data.as_bytes(), |e| {
            entries.push(e);
            Ok(())
        })
        .expect("parse");
        assert_eq!(stats.entries, 3);
        assert_eq!(entries[0].name, "a,b.mkv");
        assert_eq!(entries[0].extension, "mkv");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].parent, "/a/");
    }
    #[test]
    fn missing_and_bad_rows() {
        let stats = parse(&b"Filename\n/a\n\n\xff\n/b\n"[..], |_| Ok(())).expect("recover");
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.skipped, 1);
        assert!(parse(&b"Size\n1\n"[..], |_| Ok(())).is_err());
    }
}
