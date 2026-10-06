use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub parent: String,
    pub extension: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub attributes: Option<u32>,
    pub is_dir: bool,
}

impl Entry {
    pub fn modified_key(&self) -> Option<i64> {
        modified_key(self.modified.as_deref()?)
    }

    pub fn display_modified(&self) -> String {
        self.modified_key()
            .and_then(filetime_date)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    }
    pub fn new(
        path: String,
        size: Option<u64>,
        modified: Option<String>,
        attributes: Option<u32>,
    ) -> Self {
        let linux = path.starts_with('/') && !path.starts_with("//");
        let separator = |c| c == '/' || (!linux && c == '\\');
        let trimmed = path.trim_end_matches(separator);
        let (parent, name) = match trimmed.rfind(separator) {
            Some(i) => (&trimmed[..=i], &trimmed[i + 1..]),
            None => ("", trimmed),
        };
        let is_dir = attributes.is_some_and(|a| a & 16 != 0) || path.ends_with(separator);
        let extension = if is_dir {
            String::new()
        } else {
            name.rsplit_once('.')
                .filter(|(stem, _)| !stem.is_empty())
                .map(|(_, ext)| ext.to_lowercase())
                .unwrap_or_default()
        };
        Self {
            name: name.to_owned(),
            parent: parent.to_owned(),
            extension,
            path,
            size,
            modified,
            attributes,
            is_dir,
        }
    }

    pub fn display_size(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        let Some(size) = self.size else {
            return String::new();
        };
        let mut value = size as f64;
        let mut unit = "B";
        for next in ["KiB", "MiB", "GiB", "TiB", "PiB"] {
            if value < 1024.0 {
                break;
            }
            value /= 1024.0;
            unit = next;
        }
        if unit == "B" {
            format!("{size} B")
        } else {
            format!("{value:.1} {unit}")
        }
    }
}

const FILETIME_EPOCH: i64 = 116_444_736_000_000_000;

fn filetime_date(ticks: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::Datelike;
    let unix = ticks.checked_sub(FILETIME_EPOCH)?;
    let date = chrono::DateTime::from_timestamp(
        unix.div_euclid(10_000_000),
        (unix.rem_euclid(10_000_000) * 100) as u32,
    )?;
    (ticks > 0 && (1601..=9999).contains(&date.year())).then_some(date)
}

/// Everything EFU dates are Windows FILETIME: 100 ns ticks since 1601-01-01 UTC.
pub fn modified_key(value: &str) -> Option<i64> {
    let ticks = value.trim().parse().ok()?;
    filetime_date(ticks).map(|_| ticks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filetime_dates_and_invalid_values() {
        let entry = Entry::new("/a".into(), None, Some("116444736000000000".into()), None);
        assert_eq!(
            filetime_date(entry.modified_key().unwrap())
                .unwrap()
                .to_rfc3339(),
            "1970-01-01T00:00:00+00:00"
        );
        assert!(!entry.display_modified().is_empty());
        assert_eq!(
            filetime_date(116444735999999999)
                .unwrap()
                .timestamp_subsec_nanos(),
            999999900
        );
        for value in [
            "",
            "bad",
            "0",
            "-1",
            "18446744073709551615",
            "9223372036854775807",
        ] {
            assert!(modified_key(value).is_none(), "{value}");
        }
        assert_eq!(
            Entry::new("/a".into(), None, None, None).display_modified(),
            ""
        );
    }
    #[test]
    fn linux_backslashes_and_size() {
        let file = Entry::new("/nas/a\\b.mkv".into(), Some(1024), None, None);
        assert_eq!(file.name, "a\\b.mkv");
        assert_eq!(file.parent, "/nas/");
        assert_eq!(file.display_size(), "1.0 KiB");
        let directory = Entry::new("/nas/目录".into(), Some(0), None, Some(16));
        assert!(directory.is_dir);
        assert_eq!(directory.display_size(), "");
        let ending_backslash = Entry::new("/nas/a\\".into(), None, None, None);
        assert!(!ending_backslash.is_dir);
    }
}
