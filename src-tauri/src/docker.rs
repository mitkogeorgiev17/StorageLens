//! What lives inside Docker's disk image, and removing what is unused (#168).
//!
//! Docker keeps every image, container, volume and build cache record in one
//! disk image (`Docker.raw` on macOS), which StorageLens never touches as a
//! file. Instead it asks the local Docker daemon, through the `docker` CLI,
//! what is in there (`docker system df -v`), and removes what the person picks
//! the same way (`docker image rm`, `container rm`, `volume rm`, `builder
//! prune`). The CLI talks to the daemon over its local socket or pipe; nothing
//! here reaches the network.
//!
//! None of this can go to the Trash: Docker deletes for good. Items in use
//! are never offered, and a removal is checked against a fresh listing first,
//! so it can only act on something the listing still calls unused. Items that
//! are unused and older than [`OLD_DAYS`] start selected, except volumes:
//! they hold data nothing can rebuild, so they are never preselected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Unused this long (by creation, or by last use for build cache) counts as old.
pub const OLD_DAYS: i64 = 30;
const DAY: i64 = 86_400;

/// The reference of the build-cache row for records unused for [`OLD_DAYS`].
pub const BUILD_OLD: &str = "build-cache-old";
/// The reference of the build-cache row for the other unused records.
pub const BUILD_RECENT: &str = "build-cache-recent";

/// The Docker section of Cleanup.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerDto {
    /// `ready`, `missing` (no docker CLI) or `unavailable` (daemon not answering).
    pub status: String,
    /// Why it is not ready, as the CLI said it.
    pub message: Option<String>,
    /// Biggest first within a kind: containers, images, volumes, build cache.
    pub items: Vec<DockerItemDto>,
    pub old_days: i64,
}

/// One thing inside Docker.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerItemDto {
    /// `container`, `image`, `volume`, `build`.
    pub kind: String,
    /// What removal names it by: a container or image id, `repo:tag`, a volume
    /// name, or [`BUILD_OLD`] / [`BUILD_RECENT`].
    pub reference: String,
    pub name: String,
    /// One line on what it is: status, base images, use count.
    pub detail: String,
    /// What removing it frees, as Docker counts it (decimal units).
    pub bytes: u64,
    /// In use by a container (or running): never offered.
    pub in_use: bool,
    /// Unix seconds: last use for build cache, creation otherwise.
    pub when: Option<i64>,
    /// Starts selected: unused, old, and not a volume.
    pub selected: bool,
}

/// One thing to remove, as the UI names it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerTarget {
    pub kind: String,
    pub reference: String,
}

/// What a removal did, per target.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerRemovalDto {
    pub removed: Vec<String>,
    pub failed: Vec<DockerFailureDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerFailureDto {
    pub reference: String,
    pub reason: String,
}

// --- `docker system df -v --format json` ------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct SystemDf {
    images: Vec<Image>,
    containers: Vec<Container>,
    volumes: Vec<Volume>,
    build_cache: Vec<BuildRecord>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Image {
    #[serde(rename = "ID")]
    id: String,
    repository: String,
    tag: String,
    containers: String,
    created_at: String,
    size: String,
    unique_size: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Container {
    #[serde(rename = "ID")]
    id: String,
    names: String,
    image: String,
    state: String,
    status: String,
    created_at: String,
    size: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Volume {
    name: String,
    links: String,
    size: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct BuildRecord {
    description: String,
    in_use: String,
    last_used_at: String,
    created_at: String,
    size: String,
}

/// Docker's human sizes ("13.5MB", "41kB", "0B (virtual 1GB)", "N/A") in
/// bytes. Docker writes them in decimal units.
fn parse_size(text: &str) -> u64 {
    let text = text.split_whitespace().next().unwrap_or("");
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let Ok(number) = text[..split].parse::<f64>() else {
        return 0;
    };
    let scale = match text[split..].to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        _ => return 0,
    };
    (number * scale).round() as u64
}

/// "2026-08-04 21:29:13.028 +0000 UTC" in unix seconds.
fn parse_time(text: &str) -> Option<i64> {
    let mut parts = text.split_whitespace();
    let date = parts.next()?;
    let clock = parts.next()?;
    let offset = parts.next().unwrap_or("+0000");
    let mut ymd = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let clock = clock.split('.').next()?;
    let mut hms = clock.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (hms.next()??, hms.next()??, hms.next()??);
    let (sign, digits) = match offset.as_bytes().first()? {
        b'+' => (1, &offset[1..]),
        b'-' => (-1, &offset[1..]),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let offset =
        sign * (digits[..2].parse::<i64>().ok()? * 3600 + digits[2..].parse::<i64>().ok()? * 60);
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * DAY + hour * 3600 + minute * 60 + second - offset)
}

/// A base image a build pulled, short: "maven:3.9-eclipse-temurin-21".
fn pulled_image(description: &str) -> Option<&str> {
    let image = description.strip_prefix("pulled from ")?;
    let image = image.split('@').next().unwrap_or(image);
    Some(
        image
            .strip_prefix("docker.io/library/")
            .or_else(|| image.strip_prefix("docker.io/"))
            .unwrap_or(image),
    )
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The rows of a `docker system df -v --format json` listing, as of `now`.
fn items(df: &SystemDf, now: i64) -> Vec<DockerItemDto> {
    let old = |when: Option<i64>| when.is_some_and(|when| now - when >= OLD_DAYS * DAY);
    let mut out = Vec::new();

    let mut containers: Vec<DockerItemDto> = df
        .containers
        .iter()
        .map(|container| {
            let when = parse_time(&container.created_at);
            let in_use = matches!(
                container.state.as_str(),
                "running" | "paused" | "restarting"
            );
            DockerItemDto {
                kind: "container".to_owned(),
                reference: container.id.clone(),
                name: container.names.clone(),
                detail: format!("{} · {}", container.image, container.status),
                bytes: parse_size(&container.size),
                in_use,
                when,
                selected: !in_use && old(when),
            }
        })
        .collect();

    let mut images: Vec<DockerItemDto> = df
        .images
        .iter()
        .map(|image| {
            let when = parse_time(&image.created_at);
            let used = image.containers.trim().parse::<u64>().unwrap_or(0);
            let dangling = image.repository == "<none>" || image.repository.is_empty();
            let (reference, name) = if dangling || image.tag == "<none>" {
                (
                    image.id.clone(),
                    format!("Untagged image {}", short_id(&image.id)),
                )
            } else {
                let name = format!("{}:{}", image.repository, image.tag);
                (name.clone(), name)
            };
            let unique = parse_size(&image.unique_size);
            let total = parse_size(&image.size);
            DockerItemDto {
                kind: "image".to_owned(),
                reference,
                name,
                detail: if used > 0 {
                    format!(
                        "Used by {}",
                        plural(used as usize, "container", "containers")
                    )
                } else if unique < total {
                    format!(
                        "{} on its own; the rest is shared with other images",
                        human(unique)
                    )
                } else {
                    "Not used by any container".to_owned()
                },
                bytes: unique,
                in_use: used > 0,
                when,
                selected: used == 0 && (dangling || old(when)),
            }
        })
        .collect();

    let mut volumes: Vec<DockerItemDto> = df
        .volumes
        .iter()
        .map(|volume| {
            let links = volume.links.trim().parse::<u64>().unwrap_or(0);
            DockerItemDto {
                kind: "volume".to_owned(),
                reference: volume.name.clone(),
                name: volume.name.clone(),
                detail: if links > 0 {
                    format!(
                        "Mounted by {}",
                        plural(links as usize, "container", "containers")
                    )
                } else {
                    "Not mounted by any container. Its data cannot be rebuilt.".to_owned()
                },
                bytes: parse_size(&volume.size),
                in_use: links > 0,
                when: None,
                selected: false,
            }
        })
        .collect();

    for list in [&mut containers, &mut images, &mut volumes] {
        list.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    }
    out.append(&mut containers);
    out.append(&mut images);
    out.append(&mut volumes);
    out.extend(build_rows(&df.build_cache, now));
    out
}

/// Build cache as two rows: unused for [`OLD_DAYS`], and the rest unused.
/// Records in use are left out; prune never takes them.
fn build_rows(records: &[BuildRecord], now: i64) -> Vec<DockerItemDto> {
    let mut rows = Vec::new();
    for (reference, wants_old) in [(BUILD_OLD, true), (BUILD_RECENT, false)] {
        let picked: Vec<(&BuildRecord, Option<i64>)> = records
            .iter()
            .filter(|record| record.in_use != "true")
            .map(|record| {
                let when =
                    parse_time(&record.last_used_at).or_else(|| parse_time(&record.created_at));
                (record, when)
            })
            .filter(|(_, when)| {
                let old = when.is_none_or(|when| now - when >= OLD_DAYS * DAY);
                old == wants_old
            })
            .collect();
        if picked.is_empty() {
            continue;
        }
        let mut bases: HashMap<&str, u64> = HashMap::new();
        for (record, _) in &picked {
            if let Some(image) = pulled_image(&record.description) {
                *bases.entry(image).or_default() += parse_size(&record.size);
            }
        }
        let mut bases: Vec<(&str, u64)> = bases.into_iter().collect();
        bases.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let layers = plural(picked.len(), "record", "records");
        let detail = if bases.is_empty() {
            format!("{layers} left by past docker builds")
        } else {
            let names: Vec<&str> = bases.iter().take(3).map(|(name, _)| *name).collect();
            let more = bases.len().saturating_sub(3);
            format!(
                "{layers} left by past docker builds · base images {}{}",
                names.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        };
        let last = picked.iter().filter_map(|(_, when)| *when).max();
        rows.push(DockerItemDto {
            kind: "build".to_owned(),
            reference: reference.to_owned(),
            name: if wants_old {
                format!("Build cache unused for {OLD_DAYS}+ days")
            } else {
                "Build cache used recently".to_owned()
            },
            detail,
            bytes: picked
                .iter()
                .map(|(record, _)| parse_size(&record.size))
                .sum(),
            in_use: false,
            when: last,
            selected: wants_old,
        });
    }
    rows
}

fn short_id(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    &id[..id.len().min(12)]
}

fn human(bytes: u64) -> String {
    let (value, unit) = match bytes {
        b if b >= 1_000_000_000 => (b as f64 / 1e9, "GB"),
        b if b >= 1_000_000 => (b as f64 / 1e6, "MB"),
        b if b >= 1_000 => (b as f64 / 1e3, "kB"),
        b => (b as f64, "B"),
    };
    format!("{value:.1} {unit}")
}

// --- Running the CLI ---------------------------------------------------------

/// Runs `docker <args>`: stdout, or stderr as the error.
pub type Run<'a> = &'a dyn Fn(&[&str]) -> Result<String, String>;

/// Where the CLI is. An app opened from the Finder gets a bare PATH, so the
/// usual install places are tried first.
fn docker_binary() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Some(files) = std::env::var_os("ProgramFiles") {
            candidates.push(Path::new(&files).join(r"Docker\Docker\resources\bin\docker.exe"));
        }
    } else {
        candidates.extend(
            [
                "/usr/local/bin/docker",
                "/opt/homebrew/bin/docker",
                "/Applications/Docker.app/Contents/Resources/bin/docker",
            ]
            .map(PathBuf::from),
        );
        if let Some(home) = &home {
            candidates.push(home.join(".docker/bin/docker"));
        }
    }
    let exe = if cfg!(windows) {
        "docker.exe"
    } else {
        "docker"
    };
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(exe)));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The real CLI, or `None` when it is not installed.
pub fn system_run() -> Option<impl Fn(&[&str]) -> Result<String, String>> {
    let binary = docker_binary()?;
    // Plugins (buildx, for builder prune) and helpers sit next to the CLI.
    let mut path = vec![binary.parent().map(Path::to_path_buf).unwrap_or_default()];
    if let Some(existing) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&existing));
    }
    let path = std::env::join_paths(path).ok();
    Some(move |args: &[&str]| {
        let mut command = Command::new(&binary);
        command.args(args);
        if let Some(path) = &path {
            command.env("PATH", path);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|error| format!("docker could not be run: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(stderr
                .trim()
                .lines()
                .last()
                .unwrap_or("docker failed")
                .to_owned())
        }
    })
}

fn listing(run: Run<'_>) -> Result<SystemDf, String> {
    let text = run(&["system", "df", "-v", "--format", "json"])?;
    serde_json::from_str(&text)
        .map_err(|error| format!("docker's listing could not be read: {error}"))
}

/// What is inside Docker, or why that cannot be said.
pub fn contents(run: Option<Run<'_>>, now: i64) -> DockerDto {
    let not = |status: &str, message: String| DockerDto {
        status: status.to_owned(),
        message: Some(message),
        items: Vec::new(),
        old_days: OLD_DAYS,
    };
    let Some(run) = run else {
        return not("missing", "Docker is not installed.".to_owned());
    };
    match listing(run) {
        Ok(df) => DockerDto {
            status: "ready".to_owned(),
            message: None,
            items: items(&df, now),
            old_days: OLD_DAYS,
        },
        Err(message) => not("unavailable", message),
    }
}

/// Remove `targets`, each only if a fresh listing still has it unused.
pub fn remove(
    run: Run<'_>,
    targets: &[DockerTarget],
    now: i64,
) -> Result<DockerRemovalDto, String> {
    let current = items(&listing(run)?, now);
    let mut result = DockerRemovalDto {
        removed: Vec::new(),
        failed: Vec::new(),
    };
    // Containers first: an image cannot go while a container still uses it.
    for kind in ["container", "image", "volume", "build"] {
        for target in targets.iter().filter(|target| target.kind == kind) {
            let found = current
                .iter()
                .find(|item| item.kind == target.kind && item.reference == target.reference);
            let outcome = match found {
                None => Err("no longer there".to_owned()),
                Some(item) if item.in_use => Err("in use".to_owned()),
                Some(item) => {
                    let until = format!("until={}h", OLD_DAYS * 24);
                    let args: Vec<&str> = match (kind, item.reference.as_str()) {
                        ("container", reference) => vec!["container", "rm", reference],
                        ("image", reference) => vec!["image", "rm", reference],
                        ("volume", reference) => vec!["volume", "rm", reference],
                        (_, BUILD_OLD) => {
                            vec!["builder", "prune", "--all", "--force", "--filter", &until]
                        }
                        _ => vec!["builder", "prune", "--all", "--force"],
                    };
                    run(&args).map(drop)
                }
            };
            match outcome {
                Ok(()) => result.removed.push(target.reference.clone()),
                Err(reason) => result.failed.push(DockerFailureDto {
                    reference: target.reference.clone(),
                    reason,
                }),
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    const NOW: i64 = 1_793_550_000; // 2026-11-01

    const LISTING: &str = r#"{
      "Images": [
        {"ID":"sha256:aaa111222333444","Repository":"postgres","Tag":"16","Containers":"1",
         "CreatedAt":"2026-01-01 10:00:00 +0000 UTC","Size":"400MB","UniqueSize":"400MB"},
        {"ID":"sha256:bbb111222333444","Repository":"node","Tag":"20","Containers":"0",
         "CreatedAt":"2026-05-01 10:00:00 +0200 CEST","Size":"1.1GB","UniqueSize":"900MB"},
        {"ID":"sha256:ccc111222333444","Repository":"<none>","Tag":"<none>","Containers":"0",
         "CreatedAt":"2026-10-30 10:00:00 +0000 UTC","Size":"50MB","UniqueSize":"50MB"},
        {"ID":"sha256:ddd111222333444","Repository":"app","Tag":"dev","Containers":"0",
         "CreatedAt":"2026-10-30 10:00:00 +0000 UTC","Size":"10MB","UniqueSize":"10MB"}
      ],
      "Containers": [
        {"ID":"c1","Names":"db","Image":"postgres:16","State":"running","Status":"Up 2 days",
         "CreatedAt":"2026-01-01 10:00:00 +0000 UTC","Size":"1.5MB (virtual 401MB)"},
        {"ID":"c2","Names":"old-test","Image":"node:20","State":"exited","Status":"Exited (0) 3 months ago",
         "CreatedAt":"2026-07-01 10:00:00 +0000 UTC","Size":"20kB (virtual 900MB)"}
      ],
      "Volumes": [
        {"Name":"pgdata","Links":"1","Size":"2GB"},
        {"Name":"orphan","Links":"0","Size":"300MB"}
      ],
      "BuildCache": [
        {"Description":"pulled from docker.io/library/maven:3.9@sha256:c07","InUse":"false",
         "LastUsedAt":"2026-08-04 21:29:13.03 +0000 UTC","CreatedAt":"2026-08-04 21:29:13 +0000 UTC","Size":"139MB"},
        {"Description":"[build 6/7] COPY src ./src","InUse":"false",
         "LastUsedAt":"2026-08-05 21:29:13 +0000 UTC","CreatedAt":"2026-08-04 21:29:13 +0000 UTC","Size":"1GB"},
        {"Description":"pulled from docker.io/library/python:3.12-slim@sha256:22","InUse":"false",
         "LastUsedAt":"2026-10-31 10:00:00 +0000 UTC","CreatedAt":"2026-10-31 10:00:00 +0000 UTC","Size":"41kB"},
        {"Description":"mount / from exec","InUse":"true",
         "LastUsedAt":"2026-01-01 10:00:00 +0000 UTC","CreatedAt":"2026-01-01 10:00:00 +0000 UTC","Size":"5GB"}
      ]
    }"#;

    fn df() -> SystemDf {
        serde_json::from_str(LISTING).expect("listing")
    }

    fn find<'a>(items: &'a [DockerItemDto], reference: &str) -> &'a DockerItemDto {
        items
            .iter()
            .find(|item| item.reference == reference)
            .expect(reference)
    }

    #[test]
    fn sizes_are_decimal_and_ignore_the_virtual_part() {
        assert_eq!(parse_size("13.5MB"), 13_500_000);
        assert_eq!(parse_size("41kB"), 41_000);
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("1.5MB (virtual 401MB)"), 1_500_000);
        assert_eq!(parse_size("2GB"), 2_000_000_000);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn times_honour_the_offset() {
        assert_eq!(parse_time("1970-01-01 00:00:00 +0000 UTC"), Some(0));
        assert_eq!(
            parse_time("2026-08-04 21:29:13.028018882 +0000 UTC"),
            Some(1_785_878_953)
        );
        assert_eq!(parse_time("1970-01-01 02:00:00 +0200 CEST"), Some(0));
        assert_eq!(parse_time("1969-12-31 19:00:00 -0500 EST"), Some(0));
        assert_eq!(parse_time("garbage"), None);
    }

    #[test]
    fn in_use_is_never_selected_and_volumes_never_start_selected() {
        let items = items(&df(), NOW);
        let kinds: Vec<&str> = items.iter().map(|item| item.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "container",
                "container",
                "image",
                "image",
                "image",
                "image",
                "volume",
                "volume",
                "build",
                "build"
            ]
        );
        let db = find(&items, "c1");
        assert!(db.in_use && !db.selected);
        let stopped = find(&items, "c2");
        assert!(!stopped.in_use && stopped.selected);
        assert_eq!(stopped.bytes, 20_000);

        assert!(find(&items, "postgres:16").in_use);
        let node = find(&items, "node:20");
        assert!(node.selected, "unused and six months old");
        assert_eq!(node.bytes, 900_000_000, "only what it does not share");
        let dangling = find(&items, "sha256:ccc111222333444");
        assert!(dangling.selected, "untagged images go even when new");
        assert_eq!(dangling.name, "Untagged image ccc111222333");
        assert!(!find(&items, "app:dev").selected, "unused but new");

        assert!(find(&items, "pgdata").in_use);
        let orphan = find(&items, "orphan");
        assert!(!orphan.in_use && !orphan.selected);
    }

    #[test]
    fn build_cache_splits_by_last_use_and_skips_records_in_use() {
        let items = items(&df(), NOW);
        let old = find(&items, BUILD_OLD);
        assert_eq!(old.bytes, 1_139_000_000);
        assert!(old.selected);
        assert_eq!(
            old.detail,
            "2 records left by past docker builds · base images maven:3.9"
        );
        let recent = find(&items, BUILD_RECENT);
        assert_eq!(recent.bytes, 41_000);
        assert!(!recent.selected);
    }

    #[test]
    fn missing_or_stopped_docker_says_so() {
        assert_eq!(contents(None, NOW).status, "missing");
        let down = |_: &[&str]| Err("Cannot connect to the Docker daemon".to_owned());
        let dto = contents(Some(&down), NOW);
        assert_eq!(dto.status, "unavailable");
        assert_eq!(
            dto.message.as_deref(),
            Some("Cannot connect to the Docker daemon")
        );
    }

    #[test]
    fn removal_checks_a_fresh_listing_and_removes_containers_first() {
        let calls = RefCell::new(Vec::<String>::new());
        let run = |args: &[&str]| {
            let line = args.join(" ");
            if line.starts_with("system df") {
                return Ok(LISTING.to_owned());
            }
            calls.borrow_mut().push(line.clone());
            if line == "image rm app:dev" {
                Err("Error: conflict".to_owned())
            } else {
                Ok(String::new())
            }
        };
        let target = |kind: &str, reference: &str| DockerTarget {
            kind: kind.to_owned(),
            reference: reference.to_owned(),
        };
        let result = remove(
            &run,
            &[
                target("image", "node:20"),
                target("image", "app:dev"),
                target("container", "c2"),
                target("container", "c1"),
                target("volume", "gone"),
                target("build", BUILD_OLD),
            ],
            NOW,
        )
        .expect("remove");
        assert_eq!(
            *calls.borrow(),
            [
                "container rm c2",
                "image rm node:20",
                "image rm app:dev",
                "builder prune --all --force --filter until=720h",
            ]
        );
        assert_eq!(result.removed, ["c2", "node:20", BUILD_OLD]);
        let failed: Vec<(&str, &str)> = result
            .failed
            .iter()
            .map(|failure| (failure.reference.as_str(), failure.reason.as_str()))
            .collect();
        assert_eq!(
            failed,
            [
                ("c1", "in use"),
                ("app:dev", "Error: conflict"),
                ("gone", "no longer there")
            ]
        );
    }
}
