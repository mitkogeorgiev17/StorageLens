//! Local Time Machine snapshots (#32): reported on the Cleanup screen, never
//! deleted by StorageLens.
//!
//! APFS snapshots are not files, so they are not in the scan and no glob rule
//! can see them. They are listed with `tmutil listlocalsnapshots <volume>`,
//! whose output is a header line and one snapshot name per line:
//!
//! ```text
//! Snapshots for disk /:
//! com.apple.TimeMachine.2026-10-31-101233.local
//! ```
//!
//! Removing one is `tmutil deletelocalsnapshots <date>`, not a move to the
//! Trash, so it falls outside StorageLens's Trash-only rule (PLAN §15.1): the
//! card explains it and the user runs it. macOS also deletes them by itself
//! when space runs low. `tmutil` reports no sizes; the space they hold shows
//! up as purgeable.
//!
//! [`parse`] is pure and runs on every platform; [`list`] runs `tmutil`
//! (macOS only).

/// The Cleanup card's rule id.
pub const RULE_ID: &str = "macos.tm_local_snapshots";
/// The Cleanup card's title.
pub const TITLE: &str = "Local Time Machine snapshots";
/// What the card says.
pub const EXPLAIN: &str = "Snapshots Time Machine keeps on this disk between backups. macOS \
    deletes them by itself when the space is needed, and they cannot be moved to the Trash. \
    To remove one now, run tmutil deletelocalsnapshots with its date.";

/// One local snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSnapshot {
    /// As `tmutil` prints it, e.g. `com.apple.TimeMachine.2026-10-31-101233.local`.
    pub name: String,
    /// For Time Machine's own snapshots, the date `deletelocalsnapshots`
    /// takes: `2026-10-31-101233`. Other tools' snapshots (system updates,
    /// backup apps) have none.
    pub date: Option<String>,
}

/// Parse `tmutil listlocalsnapshots` output.
pub fn parse(output: &str) -> Vec<LocalSnapshot> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("Snapshots for disk"))
        .map(|line| LocalSnapshot {
            name: line.to_owned(),
            date: time_machine_date(line),
        })
        .collect()
}

/// `YYYY-MM-DD-HHMMSS` from a Time Machine snapshot name.
fn time_machine_date(name: &str) -> Option<String> {
    let rest = name.strip_prefix("com.apple.TimeMachine.")?;
    let date = rest.strip_suffix(".local").unwrap_or(rest);
    let well_formed = date.len() == 17
        && date.bytes().enumerate().all(|(i, byte)| match i {
            4 | 7 | 10 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    well_formed.then(|| date.to_owned())
}

/// The local snapshots of the volume mounted at `volume`, from `tmutil`.
#[cfg(target_os = "macos")]
pub fn list(volume: &std::path::Path) -> std::io::Result<Vec<LocalSnapshot>> {
    let output = std::process::Command::new("/usr/bin/tmutil")
        .arg("listlocalsnapshots")
        .arg(volume)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_output_with_no_snapshots_is_empty() {
        // Captured from `tmutil listlocalsnapshots /` on macOS 27.0.1.
        assert_eq!(parse(include_str!("../tests/fixtures/tmutil-none.txt")), []);
        assert_eq!(parse(""), []);
    }

    #[test]
    fn snapshots_are_listed_with_their_dates() {
        let snapshots = parse(include_str!("../tests/fixtures/tmutil-three.txt"));
        let dates: Vec<_> = snapshots.iter().map(|s| s.date.as_deref()).collect();
        assert_eq!(
            dates,
            [Some("2026-10-30-091502"), Some("2026-10-31-101233"), None]
        );
        assert_eq!(snapshots[2].name, "com.apple.os.update-4C3F0B1D9E8A");
        // Older macOS prints names without `.local`.
        assert_eq!(
            parse("com.apple.TimeMachine.2019-07-15-093045\n")[0]
                .date
                .as_deref(),
            Some("2019-07-15-093045")
        );
        assert_eq!(parse("com.apple.TimeMachine.2019-07-15\n")[0].date, None);
        assert_eq!(
            parse("com.apple.TimeMachine.2019x07-15-093045\n")[0].date,
            None
        );
        assert_eq!(
            parse("com.apple.TimeMachine.2019-07-15-09304x\n")[0].date,
            None
        );
    }
}
