//! Release notes extraction shared by CI and the release helper. No GTK dependency.
use anyhow::{Result, bail};

/// Extract exactly one Keep a Changelog version section, excluding its heading.
pub fn release_notes(markdown: &str, version: &str) -> Result<String> {
    let mut notes = Vec::new();
    let mut active = false;
    let mut matches = 0;
    let mut fence: Option<(char, usize)> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next();
        let length = marker.map_or(0, |c| trimmed.chars().take_while(|next| *next == c).count());
        if let Some((character, minimum)) = fence {
            if marker == Some(character) && length >= minimum && trimmed[length..].trim().is_empty()
            {
                fence = None;
            }
        } else if matches!(marker, Some('`' | '~')) && length >= 3 {
            fence = marker.map(|character| (character, length));
        } else if let Some(heading) = line.strip_prefix("## ") {
            active = false;
            if let Some(rest) = heading.strip_prefix('[')
                && let Some((candidate, suffix)) = rest.split_once(']')
                && candidate == version
                && (suffix.is_empty() || suffix.starts_with(" - "))
            {
                matches += 1;
                if matches > 1 {
                    bail!("CHANGELOG 含重复版本章节 [{version}]");
                }
                active = true;
            }
            continue;
        }
        if active {
            notes.push(line);
        }
    }
    if matches == 0 {
        bail!("CHANGELOG 缺少版本章节 [{version}]");
    }
    let notes = notes.join("\n").trim().to_owned();
    if notes.is_empty() {
        bail!("CHANGELOG 的 [{version}] 章节为空");
    }
    Ok(format!("{notes}\n"))
}

pub fn notes_for_tag(markdown: &str, tag: &str) -> Result<String> {
    let expected = format!("v{}", crate::VERSION);
    if tag != expected {
        bail!("标签 {tag:?} 与 Cargo.toml 版本不一致，预期 {expected}");
    }
    release_notes(markdown, crate::VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_matching_version_and_ignore_fenced_headings() -> Result<()> {
        let markdown = "# Changes\n## [Unreleased]\nfuture\n## [0.1.0] - 2026-10-06\n\n### Added\n- feature\n```md\n## [9.9.9]\n```\n\n## [0.0.1]\nold\n";
        assert_eq!(
            release_notes(markdown, "0.1.0")?,
            "### Added\n- feature\n```md\n## [9.9.9]\n```\n"
        );
        Ok(())
    }
    #[test]
    fn missing_empty_duplicate_and_wrong_tag_fail() {
        assert!(release_notes("## [Unreleased]\nfuture", "0.1.0").is_err());
        assert!(release_notes("## [0.1.0]\n\n## [0.0.1]\nold", "0.1.0").is_err());
        assert!(release_notes("## [0.1.0]\nfirst\n## [0.1.0]\nsecond", "0.1.0").is_err());
        assert!(notes_for_tag("## [0.1.0]\nrelease", "v9.9.9").is_err());
    }
    #[test]
    fn current_release_and_title_follow_cargo() -> Result<()> {
        let tag = format!("v{}", crate::VERSION);
        assert!(!notes_for_tag(include_str!("../CHANGELOG.md"), &tag)?.is_empty());
        assert_eq!(crate::WINDOW_TITLE, format!("EfuSeek {tag}"));
        Ok(())
    }
}
