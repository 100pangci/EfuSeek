//! CI helper: fail on missing/empty notes or a tag that disagrees with Cargo.toml.
use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let tag = args
        .next()
        .context("usage: release_notes TAG OUTPUT_FILE")?;
    let output = args.next().context("missing output file")?;
    if args.next().is_some() {
        bail!("unexpected argument");
    }
    let tag = tag.to_str().context("tag must be UTF-8")?;
    let notes = efuseek::changelog::notes_for_tag(include_str!("../CHANGELOG.md"), tag)?;
    std::fs::write(output, notes)?;
    println!("Release notes verified for {tag}");
    Ok(())
}
