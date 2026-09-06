use anyhow::{Context, Result, bail};
use chrono::Local;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const CONFIG_FILE_NAME: &str = "config.yaml";

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    source_directory: Option<PathBuf>,
    #[serde(default)]
    output_directory: Option<PathBuf>,
    #[serde(default)]
    process_all_folders: bool,
    #[serde(default)]
    source_root_directory: Option<PathBuf>,
    #[serde(default)]
    output_root_directory: Option<PathBuf>,
    #[serde(default)]
    blacklist: Vec<String>,
}

#[derive(Default)]
struct Blacklist {
    patterns: Vec<Regex>,
}

impl Blacklist {
    fn from_patterns(patterns: &[String]) -> Result<Self> {
        let patterns = patterns
            .iter()
            .map(|pattern| {
                Regex::new(pattern).with_context(|| format!("Invalid blacklist regex: {pattern}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { patterns })
    }

    fn matches(&self, path: &Path) -> bool {
        path.components().any(|component| {
            let component = component.as_os_str().to_string_lossy();
            self.patterns
                .iter()
                .any(|pattern| pattern.is_match(&component))
        })
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let application_directory = env::current_exe()?
        .parent()
        .context("The application path has no parent directory")?
        .to_path_buf();
    let config_path = application_directory.join(CONFIG_FILE_NAME);
    let config = read_config(&config_path)?;
    let configured_source = if config.process_all_folders {
        config
            .source_root_directory
            .as_deref()
            .context("source_root_directory is required when process_all_folders is true")?
    } else {
        config
            .source_directory
            .as_deref()
            .context("source_directory is required when process_all_folders is false")?
    };
    let configured_output = if config.process_all_folders {
        config
            .output_root_directory
            .as_deref()
            .context("output_root_directory is required when process_all_folders is true")?
    } else {
        config
            .output_directory
            .as_deref()
            .context("output_directory is required when process_all_folders is false")?
    };
    let source_directory = resolve_path(&application_directory, configured_source);
    let output_directory = resolve_path(&application_directory, configured_output);
    let blacklist = Blacklist::from_patterns(&config.blacklist)?;

    if !source_directory.is_dir() {
        bail!(
            "Source directory does not exist or is not a directory: {}",
            source_directory.display()
        );
    }
    fs::create_dir_all(&output_directory).with_context(|| {
        format!(
            "Could not create output directory {}",
            output_directory.display()
        )
    })?;

    let log_path = application_directory.join(format!(
        "changes-{}.log",
        Local::now().format("%m%d%Y_%H%M")
    ));
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Could not open log file {}", log_path.display()))?;

    writeln!(
        log,
        "START mode={} source={} output={}",
        if config.process_all_folders {
            "all-folders"
        } else {
            "single-folder"
        },
        source_directory.display(),
        output_directory.display()
    )?;
    let summary = if config.process_all_folders {
        create_hard_links_for_all_folders_with_blacklist(
            &source_directory,
            &output_directory,
            &mut log,
            &blacklist,
        )?
    } else {
        create_hard_links_with_blacklist(
            &source_directory,
            &output_directory,
            &mut log,
            &blacklist,
        )?
    };
    writeln!(
        log,
        "COMPLETE linked={} skipped={} warnings={}",
        summary.linked, summary.skipped, summary.warnings
    )?;
    println!(
        "Linked {} file(s); skipped {} file(s). Log: {}",
        summary.linked,
        summary.skipped,
        log_path.display()
    );

    Ok(())
}

fn read_config(config_path: &Path) -> Result<Config> {
    let contents = fs::read_to_string(config_path).with_context(|| {
        format!(
            "Could not read configuration file {}",
            config_path.display()
        )
    })?;
    serde_yaml::from_str(&contents).with_context(|| {
        format!(
            "Could not parse configuration file {}",
            config_path.display()
        )
    })
}

fn resolve_path(application_directory: &Path, configured_path: &Path) -> PathBuf {
    if configured_path.is_absolute() {
        configured_path.to_path_buf()
    } else {
        application_directory.join(configured_path)
    }
}

#[derive(Default)]
struct Summary {
    linked: usize,
    skipped: usize,
    warnings: usize,
}

impl Summary {
    fn add(&mut self, other: Summary) {
        self.linked += other.linked;
        self.skipped += other.skipped;
        self.warnings += other.warnings;
    }
}

#[cfg(test)]
fn create_hard_links_for_all_folders(
    source_root_directory: &Path,
    output_root_directory: &Path,
    log: &mut File,
) -> Result<Summary> {
    create_hard_links_for_all_folders_with_blacklist(
        source_root_directory,
        output_root_directory,
        log,
        &Blacklist::default(),
    )
}

fn create_hard_links_for_all_folders_with_blacklist(
    source_root_directory: &Path,
    output_root_directory: &Path,
    log: &mut File,
    blacklist: &Blacklist,
) -> Result<Summary> {
    let mut summary = Summary::default();
    summary.add(create_direct_hard_links(
        source_root_directory,
        output_root_directory,
        log,
        blacklist,
    )?);
    let mut folder_entries = read_directory_entries(source_root_directory)?;
    folder_entries.sort_by_key(|entry| entry.file_name());

    for folder_entry in folder_entries {
        if !folder_entry.file_type()?.is_dir() {
            continue;
        }

        let source_folder = folder_entry.path();
        if blacklist.matches(&source_folder) {
            writeln!(
                log,
                "SKIP folder={} reason=blacklist",
                source_folder.display()
            )?;
            summary.skipped += 1;
            continue;
        }
        let folder_name = folder_entry.file_name();
        let output_folder = output_root_directory.join(&folder_name);
        fs::create_dir_all(&output_folder).with_context(|| {
            format!(
                "Could not create folder output directory {}",
                output_folder.display()
            )
        })?;
        writeln!(
            log,
            "FOLDER source={} output={}",
            source_folder.display(),
            output_folder.display()
        )?;

        summary.add(create_hard_links_with_blacklist(
            &source_folder,
            &output_folder,
            log,
            blacklist,
        )?);
    }

    Ok(summary)
}

#[cfg(test)]
fn create_hard_links(
    source_directory: &Path,
    output_directory: &Path,
    log: &mut File,
) -> Result<Summary> {
    create_hard_links_with_blacklist(
        source_directory,
        output_directory,
        log,
        &Blacklist::default(),
    )
}

fn create_hard_links_with_blacklist(
    source_directory: &Path,
    output_directory: &Path,
    log: &mut File,
    blacklist: &Blacklist,
) -> Result<Summary> {
    let mut seasons_seen = HashSet::new();
    create_hard_links_recursive(
        source_directory,
        output_directory,
        &mut seasons_seen,
        log,
        blacklist,
    )
}

fn create_hard_links_recursive(
    source_directory: &Path,
    output_directory: &Path,
    seasons_seen: &mut HashSet<u32>,
    log: &mut File,
    blacklist: &Blacklist,
) -> Result<Summary> {
    let mut summary = Summary::default();
    let mut season_entries = read_directory_entries(source_directory)?;
    season_entries.sort_by_key(|entry| entry.file_name());
    summary.add(create_direct_hard_links(
        source_directory,
        output_directory,
        log,
        blacklist,
    )?);

    let mut processed_season_paths = HashSet::new();
    for (season_index, season_entry) in season_entries.iter().enumerate() {
        let season_path = season_entry.path();
        if !season_entry.file_type()?.is_dir() {
            continue;
        }
        if processed_season_paths.contains(&season_path) {
            continue;
        }
        if blacklist.matches(&season_path) {
            writeln!(
                log,
                "SKIP folder={} reason=blacklist",
                season_path.display()
            )?;
            summary.skipped += 1;
            continue;
        }

        let season_name = season_entry.file_name().to_string_lossy().into_owned();
        let Some((season_number, grouped_indices)) =
            find_season_folder_group(&season_entries, season_index)?
        else {
            let contains_nested_seasons = contains_season_folder(&season_path)?;
            let nested_output_directory = if contains_nested_seasons {
                writeln!(
                    log,
                    "FLATTEN source={} output={} reason=nested-season-folders",
                    season_path.display(),
                    output_directory.display()
                )?;
                output_directory.to_path_buf()
            } else {
                output_directory.join(&season_name)
            };
            summary.add(create_hard_links_recursive(
                &season_path,
                &nested_output_directory,
                seasons_seen,
                log,
                blacklist,
            )?);
            continue;
        };
        let grouped_paths = grouped_indices
            .iter()
            .map(|index| season_entries[*index].path())
            .collect::<Vec<_>>();
        processed_season_paths.extend(grouped_paths.iter().cloned());
        if !seasons_seen.insert(season_number) {
            writeln!(
                log,
                "SKIP season={} path={} reason=duplicate-season-number",
                season_number,
                season_path.display()
            )?;
            summary.skipped += 1;
            continue;
        }

        let destination_name = season_destination_name(&season_entries, &grouped_indices);
        let destination_season = output_directory.join(destination_name);
        fs::create_dir_all(&destination_season).with_context(|| {
            format!(
                "Could not create season directory {}",
                destination_season.display()
            )
        })?;

        summary.add(create_hard_links_for_season_folders(
            &grouped_paths,
            &destination_season,
            season_number,
            log,
            blacklist,
        )?);
    }

    Ok(summary)
}

fn contains_season_folder(directory: &Path) -> Result<bool> {
    let entries = read_directory_entries(directory)?;
    for (index, entry) in entries.iter().enumerate() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if find_season_folder_group(&entries, index)?.is_some()
            || contains_season_folder(&entry.path())?
        {
            return Ok(true);
        }
    }

    Ok(false)
}

fn create_hard_links_for_season_folders(
    season_paths: &[PathBuf],
    destination_season: &Path,
    season_number: u32,
    log: &mut File,
    blacklist: &Blacklist,
) -> Result<Summary> {
    let mut summary = Summary::default();
    let mut episodes_seen = HashSet::new();

    for season_path in season_paths {
        let mut episode_entries = read_directory_entries(season_path)?;
        episode_entries.sort_by_key(|entry| entry.file_name());

        for episode_entry in episode_entries {
            if !episode_entry.file_type()?.is_file() {
                continue;
            }

            let source_path = episode_entry.path();
            if blacklist.matches(&source_path) {
                writeln!(log, "SKIP file={} reason=blacklist", source_path.display())?;
                summary.skipped += 1;
                continue;
            }
            if !is_video_file(&source_path) {
                writeln!(
                    log,
                    "SKIP file={} reason=not-video-file",
                    source_path.display()
                )?;
                summary.skipped += 1;
                continue;
            }
            let source_stem = source_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("Episode filename is not valid UTF-8")?;
            let Some(episode_number) = extract_episode_number(source_stem) else {
                writeln!(
                    log,
                    "WARN skip-file path={} reason=no-episode-number",
                    source_path.display()
                )?;
                summary.warnings += 1;
                continue;
            };
            if !episodes_seen.insert(episode_number) {
                writeln!(
                    log,
                    "SKIP season={} episode={} path={} reason=duplicate-episode-number",
                    season_number,
                    episode_number,
                    source_path.display()
                )?;
                summary.skipped += 1;
                continue;
            }

            let destination_name = corrected_file_name(
                source_stem,
                season_number,
                episode_number,
                source_path.extension(),
            );
            let destination_path = destination_season.join(destination_name);
            if destination_path.exists() {
                writeln!(
                    log,
                    "SKIP season={} episode={} source={} destination={} reason=destination-exists",
                    season_number,
                    episode_number,
                    source_path.display(),
                    destination_path.display()
                )?;
                summary.skipped += 1;
                continue;
            }

            match fs::hard_link(&source_path, &destination_path) {
                Ok(()) => {
                    writeln!(
                        log,
                        "LINK season={} episode={} source={} destination={}",
                        season_number,
                        episode_number,
                        source_path.display(),
                        destination_path.display()
                    )?;
                    summary.linked += 1;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    writeln!(
                        log,
                        "SKIP season={} episode={} source={} destination={} reason=destination-exists",
                        season_number,
                        episode_number,
                        source_path.display(),
                        destination_path.display()
                    )?;
                    summary.skipped += 1;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "Could not create hard link {} -> {}",
                            source_path.display(),
                            destination_path.display()
                        )
                    });
                }
            }
        }
    }

    Ok(summary)
}

fn find_season_folder_group(
    entries: &[fs::DirEntry],
    seed_index: usize,
) -> Result<Option<(u32, Vec<usize>)>> {
    let seed_name = entries[seed_index]
        .file_name()
        .to_string_lossy()
        .into_owned();
    let seed_info = season_folder_info(&seed_name);
    let mut season_number = seed_info.season_number;
    let mut anchor_key = seed_info.title_key.clone();

    if season_number.is_none() {
        for (index, entry) in entries.iter().enumerate() {
            if index == seed_index || !entry.file_type()?.is_dir() {
                continue;
            }
            let candidate_name = entry.file_name().to_string_lossy().into_owned();
            let candidate_info = season_folder_info(&candidate_name);
            if candidate_info.part_number.is_some()
                && candidate_info.season_number.is_some_and(|_| {
                    season_folder_titles_related(&seed_info.title_key, &candidate_info.title_key)
                })
            {
                season_number = candidate_info.season_number;
                anchor_key = candidate_info.title_key;
                break;
            }
        }
    }

    let Some(season_number) = season_number else {
        return Ok(None);
    };

    let mut grouped_indices = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let candidate_name = entry.file_name().to_string_lossy().into_owned();
        let candidate_info = season_folder_info(&candidate_name);
        let same_season = candidate_info.season_number == Some(season_number);
        let related_markerless_folder = candidate_info.season_number.is_none()
            && !anchor_key.is_empty()
            && season_folder_titles_related(&anchor_key, &candidate_info.title_key);
        let related_part_folder = candidate_info.part_number.is_some()
            && (anchor_key.is_empty()
                || season_folder_titles_related(&anchor_key, &candidate_info.title_key));
        let generic_season_folder =
            same_season && candidate_info.part_number.is_none() && seed_info.part_number.is_some();

        if (same_season && related_part_folder)
            || related_markerless_folder
            || generic_season_folder
            || index == seed_index
        {
            grouped_indices.push(index);
        }
    }

    Ok(Some((season_number, grouped_indices)))
}

fn season_destination_name(
    entries: &[fs::DirEntry],
    grouped_indices: &[usize],
) -> std::ffi::OsString {
    grouped_indices
        .iter()
        .find(|index| {
            let name = entries[**index].file_name().to_string_lossy().into_owned();
            season_folder_info(&name).part_number == Some(1)
        })
        .or_else(|| {
            grouped_indices.iter().find(|index| {
                let name = entries[**index].file_name().to_string_lossy().into_owned();
                season_folder_info(&name).part_number.is_none()
            })
        })
        .map(|index| entries[*index].file_name())
        .unwrap_or_else(|| entries[grouped_indices[0]].file_name())
}

fn create_direct_hard_links(
    source_directory: &Path,
    destination_directory: &Path,
    log: &mut File,
    blacklist: &Blacklist,
) -> Result<Summary> {
    let mut summary = Summary::default();
    fs::create_dir_all(&destination_directory).with_context(|| {
        format!(
            "Could not create directory {}",
            destination_directory.display()
        )
    })?;

    let mut file_entries = read_directory_entries(source_directory)?;
    file_entries.sort_by_key(|entry| entry.file_name());
    for file_entry in file_entries {
        if !file_entry.file_type()?.is_file() {
            continue;
        }

        let source_path = file_entry.path();
        if blacklist.matches(&source_path) {
            writeln!(log, "SKIP file={} reason=blacklist", source_path.display())?;
            summary.skipped += 1;
            continue;
        }
        if !is_video_file(&source_path) {
            writeln!(
                log,
                "SKIP file={} reason=not-video-file",
                source_path.display()
            )?;
            summary.skipped += 1;
            continue;
        }

        let destination_name = match source_path.file_stem().and_then(|stem| stem.to_str()) {
            Some(source_stem) => extract_season_and_episode(source_stem)
                .map(|(season, episode)| {
                    corrected_file_name(source_stem, season, episode, source_path.extension())
                })
                .map(std::ffi::OsString::from)
                .unwrap_or_else(|| file_entry.file_name()),
            None => file_entry.file_name(),
        };
        let original_destination_path = destination_directory.join(file_entry.file_name());
        let destination_path = destination_directory.join(destination_name);
        if destination_path.exists() {
            remove_stale_original_link(
                &source_path,
                &original_destination_path,
                &destination_path,
                log,
            )?;
            writeln!(
                log,
                "SKIP source={} destination={} reason=destination-exists",
                source_path.display(),
                destination_path.display()
            )?;
            summary.skipped += 1;
            continue;
        }
        if let Some(existing_link) = find_existing_hard_link(
            &source_path,
            destination_directory,
            Some(&original_destination_path),
        )? {
            writeln!(
                log,
                "SKIP source={} destination={} reason=hard-link-exists",
                source_path.display(),
                existing_link.display()
            )?;
            summary.skipped += 1;
            continue;
        }

        match fs::hard_link(&source_path, &destination_path) {
            Ok(()) => {
                writeln!(
                    log,
                    "DIRECT-LINK source={} destination={}",
                    source_path.display(),
                    destination_path.display()
                )?;
                remove_stale_original_link(
                    &source_path,
                    &original_destination_path,
                    &destination_path,
                    log,
                )?;
                summary.linked += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                writeln!(
                    log,
                    "SKIP source={} destination={} reason=destination-exists",
                    source_path.display(),
                    destination_path.display()
                )?;
                summary.skipped += 1;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Could not create hard link {} -> {}",
                        source_path.display(),
                        destination_path.display()
                    )
                });
            }
        }
    }

    Ok(summary)
}

fn find_existing_hard_link(
    source_path: &Path,
    destination_directory: &Path,
    excluded_path: Option<&Path>,
) -> Result<Option<PathBuf>> {
    for entry in read_directory_entries(destination_directory)? {
        let candidate_path = entry.path();
        if excluded_path.is_some_and(|path| path == candidate_path) || !entry.file_type()?.is_file()
        {
            continue;
        }
        if same_file::is_same_file(source_path, &candidate_path).unwrap_or(false) {
            return Ok(Some(candidate_path));
        }
    }

    Ok(None)
}

fn remove_stale_original_link(
    source_path: &Path,
    original_destination_path: &Path,
    normalized_destination_path: &Path,
    log: &mut File,
) -> Result<()> {
    if original_destination_path == normalized_destination_path
        || !original_destination_path.exists()
        || !same_file::is_same_file(source_path, original_destination_path).unwrap_or(false)
    {
        return Ok(());
    }

    fs::remove_file(original_destination_path).with_context(|| {
        format!(
            "Could not remove stale hard link {}",
            original_destination_path.display()
        )
    })?;
    writeln!(
        log,
        "CLEANUP source={} destination={} reason=normalized-name",
        source_path.display(),
        original_destination_path.display()
    )?;
    Ok(())
}

fn read_directory_entries(directory: &Path) -> Result<Vec<fs::DirEntry>> {
    fs::read_dir(directory)
        .with_context(|| format!("Could not read directory {}", directory.display()))?
        .collect::<io::Result<Vec<_>>>()
        .with_context(|| format!("Could not enumerate directory {}", directory.display()))
}

fn extract_season_folder_number(name: &str) -> Option<u32> {
    let patterns = [
        r"^(\d{1,3})(?:$|[ ._-]+)",
        r"(?i)(?:^|[^a-z0-9])season[ ._-]*(\d{1,3})(?:$|[ ._-]+)",
        r"(?i)(?:^|[^a-z0-9])s(\d{1,3})(?:$|[ ._-]+)",
    ];

    patterns.iter().find_map(|pattern| {
        Regex::new(pattern)
            .ok()?
            .captures(name)
            .and_then(|captures| captures.get(1))
            .and_then(|number| number.as_str().parse().ok())
    })
}

struct SeasonFolderInfo {
    season_number: Option<u32>,
    part_number: Option<u32>,
    title_key: String,
}

fn season_folder_info(name: &str) -> SeasonFolderInfo {
    let season_and_part = extract_season_and_part_number(name);
    let season_number = season_and_part
        .map(|(season, _)| season)
        .or_else(|| extract_season_folder_number(name));
    let part_number = season_and_part
        .map(|(_, part)| part)
        .or_else(|| extract_standalone_part_number(name));
    let title_key = season_folder_title_key(name);

    SeasonFolderInfo {
        season_number,
        part_number,
        title_key,
    }
}

fn extract_season_and_part_number(name: &str) -> Option<(u32, u32)> {
    let patterns = [
        r"(?i)(?:season|s)[ ._-]*(\d{1,3})[ ._-]*(?:part|pt|p)[ ._-]*(\d{1,3})(?:$|[^a-z0-9])",
        r"(?i)(?:part|pt|p)[ ._-]*(\d{1,3})[ ._-]*(?:of[ ._-]*)?(?:season|s)[ ._-]*(\d{1,3})(?:$|[^a-z0-9])",
    ];

    patterns.iter().find_map(|pattern| {
        let captures = Regex::new(pattern).ok()?.captures(name)?;
        let first = captures.get(1)?.as_str().parse().ok()?;
        let second = captures.get(2)?.as_str().parse().ok()?;
        if pattern.starts_with("(?i)(?:part") {
            Some((second, first))
        } else {
            Some((first, second))
        }
    })
}

fn extract_standalone_part_number(name: &str) -> Option<u32> {
    let pattern = r"(?i)(?:^|[ ._+-])(?:part|pt)[ ._-]*(\d{1,3})(?:$|[^a-z0-9])";
    Regex::new(pattern)
        .ok()?
        .captures(name)
        .and_then(|captures| captures.get(1))
        .and_then(|number| number.as_str().parse().ok())
}

fn season_folder_title_key(name: &str) -> String {
    let mut title = name;
    let marker_patterns = [
        r"(?i)(?:season|s)[ ._-]*\d{1,3}[ ._-]*(?:part|pt|p)[ ._-]*\d{1,3}",
        r"(?i)(?:part|pt)[ ._-]*\d{1,3}",
    ];
    for pattern in marker_patterns {
        if let Ok(regex) = Regex::new(pattern) {
            if let Some(marker) = regex.find(title) {
                title = &title[..marker.start()];
                break;
            }
        }
    }

    let title = Regex::new(r"\[[^\]]*\]")
        .map(|regex| regex.replace_all(title, " ").into_owned())
        .unwrap_or_else(|_| title.to_owned());
    title
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn season_folder_titles_related(first: &str, second: &str) -> bool {
    if first.is_empty() || second.is_empty() {
        return false;
    }
    first == second
        || first
            .strip_prefix(second)
            .is_some_and(|remainder| remainder.starts_with(' '))
        || second
            .strip_prefix(first)
            .is_some_and(|remainder| remainder.starts_with(' '))
}

fn is_video_file(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };

    matches!(
        extension.to_ascii_lowercase().as_str(),
        "avi"
            | "flv"
            | "m2ts"
            | "m4v"
            | "mkv"
            | "mov"
            | "mp4"
            | "mpeg"
            | "mpg"
            | "ogv"
            | "ts"
            | "webm"
            | "wmv"
    )
}

fn extract_episode_number(stem: &str) -> Option<u32> {
    let patterns = [
        r"(?i)\bS\d{1,3}E(\d{1,3})(?:v\d+)?(?:[^a-z0-9]|$)",
        r"(?i)\b\d{1,3}x(\d{1,3})\b",
        r"(?i)\b(?:episode|ep)[ ._-]*(\d{1,3})\b",
        r"(?:^|[ ._-])(\d{1,3})(?:[ ._-]|$)",
    ];

    patterns.iter().find_map(|pattern| {
        Regex::new(pattern)
            .ok()?
            .captures(stem)
            .and_then(|captures| captures.get(1))
            .and_then(|number| number.as_str().parse().ok())
    })
}

fn extract_season_and_episode(stem: &str) -> Option<(u32, u32)> {
    Regex::new(r"(?i)\bS(\d{1,3})E(\d{1,3})(?:v\d+)?(?:[^a-z0-9]|$)")
        .ok()?
        .captures(stem)
        .and_then(|captures| {
            Some((
                captures.get(1)?.as_str().parse().ok()?,
                captures.get(2)?.as_str().parse().ok()?,
            ))
        })
}

fn corrected_file_name(
    stem: &str,
    season: u32,
    episode: u32,
    extension: Option<&std::ffi::OsStr>,
) -> String {
    let mut title = stem.to_owned();
    if let Ok(regex) = Regex::new(r"(?i)\bS\d{1,3}E\d{1,3}(v\d+)?") {
        title = regex.replace_all(&title, "$1").into_owned();
    }
    let patterns = [
        r"(?i)\bseason[ ._-]*\d{1,3}[ ._-]*(?:episode|ep|e)[ ._-]*\d{1,3}\b",
        r"(?i)\b\d{1,3}x\d{1,3}\b",
        r"(?i)\b(?:season|s)[ ._-]*\d{1,3}\b",
        r"(?i)\b(?:episode|ep|e)[ ._-]*\d{1,3}\b",
    ];
    for pattern in patterns {
        if let Ok(regex) = Regex::new(pattern) {
            title = regex.replace_all(&title, "").into_owned();
        }
    }
    if let Ok(regex) = Regex::new(&format!(r"(?i)(^|[ ._\-\[\(])0*{}($|[ ._\-\]\)])", episode)) {
        title = regex.replace_all(&title, "$1$2").into_owned();
    }

    if let Ok(regex) = Regex::new(r"\s*[-_.]\s*[-_.]\s*") {
        title = regex.replace_all(&title, " - ").into_owned();
    }
    let title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(&[' ', '-', '_', '.'][..])
        .to_owned();
    let title = if title.is_empty() { "Episode" } else { &title };
    let extension = extension
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();

    format!("[S{season:02}E{episode:02}] {title}{extension}")
}

#[cfg(test)]
mod tests {
    use super::{
        corrected_file_name, extract_episode_number, extract_season_and_episode,
        extract_season_and_part_number, extract_season_folder_number,
    };

    #[test]
    fn extracts_common_season_folder_names() {
        assert_eq!(extract_season_folder_number("Season 01"), Some(1));
        assert_eq!(extract_season_folder_number("S02"), Some(2));
        assert_eq!(
            extract_season_folder_number("[Author] Show Name - S03 v2 [1080p AV1][Dual Audio]"),
            Some(3)
        );
        assert_eq!(
            extract_season_folder_number("Show.name.S04P01.1080p.WEBRip.Dual.Audio.AV1-Author"),
            None
        );
        assert_eq!(extract_season_folder_number("03 - Show name"), Some(3));
        assert_eq!(extract_season_folder_number("13 - Show name S1"), Some(13));
        assert_eq!(extract_season_folder_number("Specials"), None);
        assert_eq!(extract_season_folder_number("Release S01+02+Movie"), None);
    }

    #[test]
    fn extracts_common_season_part_folder_names() {
        assert_eq!(
            extract_season_and_part_number("Show Name S2 P2 [1080p]"),
            Some((2, 2))
        );
        assert_eq!(
            extract_season_and_part_number("Show Name Season 02 Part 1"),
            Some((2, 1))
        );
        assert_eq!(
            extract_season_and_part_number("Show Name S02P02"),
            Some((2, 2))
        );
        assert_eq!(
            extract_season_and_part_number("Show.name.S04P01.1080p.WEBRip.Dual.Audio.AV1-Author"),
            Some((4, 1))
        );
        assert_eq!(
            extract_season_and_part_number("Show Name Part 2 of Season 2"),
            Some((2, 2))
        );
    }

    #[test]
    fn accepts_a_batch_only_configuration() {
        let config: super::Config = serde_yaml::from_str(
            "process_all_folders: true\nsource_root_directory: /source\noutput_root_directory: /output\nblacklist: ['^sample']\n",
        )
        .unwrap();

        assert!(config.process_all_folders);
        assert!(config.source_directory.is_none());
        assert!(config.output_directory.is_none());
        assert_eq!(
            config.source_root_directory.as_deref(),
            Some(std::path::Path::new("/source"))
        );
        assert_eq!(
            config.output_root_directory.as_deref(),
            Some(std::path::Path::new("/output"))
        );
        assert_eq!(config.blacklist, vec!["^sample"]);
    }

    #[test]
    fn accepts_video_files_but_rejects_metadata_files() {
        assert!(super::is_video_file(std::path::Path::new("episode.mkv")));
        assert!(super::is_video_file(std::path::Path::new("episode.MP4")));
        assert!(!super::is_video_file(std::path::Path::new("episode.nfo")));
    }

    #[test]
    fn skips_blacklisted_files_and_folders() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-blacklist-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let first_season = source.join("Season 01");
        let blacklisted_season = source.join("Season 02");
        let output = root.join("output");
        std::fs::create_dir_all(&first_season).unwrap();
        std::fs::create_dir_all(&blacklisted_season).unwrap();
        std::fs::write(first_season.join("Show - S01E01.mkv"), b"episode").unwrap();
        std::fs::write(first_season.join("sample.mkv"), b"sample").unwrap();
        std::fs::write(
            blacklisted_season.join("Show - S02E01.mkv"),
            b"blacklisted season",
        )
        .unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let blacklist = super::Blacklist::from_patterns(&[
            String::from(r"^sample\.mkv$"),
            String::from(r"^Season 02$"),
        ])
        .unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary =
            super::create_hard_links_with_blacklist(&source, &output, &mut log, &blacklist)
                .unwrap();

        assert_eq!(summary.linked, 1);
        assert_eq!(summary.skipped, 2);
        assert!(output.join("Season 01").join("[S01E01] Show.mkv").exists());
        assert!(!output.join("Season 01").join("sample.mkv").exists());
        assert!(!output.join("Season 02").exists());
        let log_contents = std::fs::read_to_string(root.join("changes.log")).unwrap();
        assert!(log_contents.contains("SKIP file="));
        assert!(log_contents.contains("sample.mkv"));
        assert!(log_contents.contains("SKIP folder="));
        assert!(log_contents.contains("Season 02"));
        assert!(log_contents.contains("reason=blacklist"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directly_links_video_files_in_an_unnumbered_season_folder() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-unnumbered-season-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let season = source.join("The Beginning");
        let output = root.join("output");
        std::fs::create_dir_all(&season).unwrap();
        std::fs::write(season.join("The Beginning Episode.mkv"), b"episode").unwrap();
        std::fs::write(season.join("The Beginning Episode.nfo"), b"metadata").unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(summary.linked, 1);
        assert_eq!(summary.skipped, 1);
        let linked_file = output
            .join("The Beginning")
            .join("The Beginning Episode.mkv");
        assert_eq!(std::fs::read(&linked_file).unwrap(), b"episode");
        assert!(
            !output
                .join("The Beginning")
                .join("The Beginning Episode.nfo")
                .exists()
        );
        std::fs::write(&linked_file, b"updated").unwrap();
        assert_eq!(
            std::fs::read(season.join("The Beginning Episode.mkv")).unwrap(),
            b"updated"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directly_links_video_files_in_the_show_root() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-show-root-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let output = root.join("output");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("Show Episode 01.mkv"), b"episode").unwrap();
        std::fs::write(source.join("Show Episode 01.nfo"), b"metadata").unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(summary.linked, 1);
        assert_eq!(summary.skipped, 1);
        let linked_file = output.join("Show Episode 01.mkv");
        assert_eq!(std::fs::read(&linked_file).unwrap(), b"episode");
        assert!(!output.join("Show Episode 01.nfo").exists());
        std::fs::write(&linked_file, b"updated").unwrap();
        assert_eq!(
            std::fs::read(source.join("Show Episode 01.mkv")).unwrap(),
            b"updated"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extracts_common_episode_formats() {
        assert_eq!(extract_episode_number("Show - S02E07 - Title"), Some(7));
        assert_eq!(
            extract_episode_number("[author] Show-Name - S05E01v2"),
            Some(1)
        );
        assert_eq!(extract_episode_number("Show 2x08 Title"), Some(8));
        assert_eq!(extract_episode_number("Show - Episode 09"), Some(9));
        assert_eq!(extract_episode_number("Show - 10"), Some(10));
        assert_eq!(
            extract_season_and_episode("[author] Show Name - S05E11v2"),
            Some((5, 11))
        );
    }

    #[test]
    fn normalizes_direct_episode_links_and_removes_stale_original_names() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-direct-episode-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let output = root.join("output");
        let original_name = "[author] Show Name - S05E11v2.mkv";
        let normalized_name = "[S05E11] [author] Show Name - v2.mkv";
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(original_name), b"episode").unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::hard_link(source.join(original_name), output.join(original_name)).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let first_summary = super::create_hard_links(&source, &output, &mut log).unwrap();
        let second_summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(first_summary.linked, 1);
        assert_eq!(second_summary.linked, 0);
        assert!(output.join(normalized_name).exists());
        assert!(!output.join(original_name).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skips_creating_original_name_when_normalized_hard_link_already_exists() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-existing-normalized-link-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let output = root.join("output");
        let original_name = "[Author] Show Name - 07 [1080p BD][AV1][dual audio].mkv";
        let normalized_name = "[S01E07] [Author] Show Name - [1080p BD][AV1][dual audio].mkv";
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(original_name), b"episode").unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::hard_link(source.join(original_name), output.join(normalized_name)).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(summary.linked, 0);
        assert_eq!(summary.skipped, 1);
        assert!(output.join(normalized_name).exists());
        assert!(!output.join(original_name).exists());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaces_existing_season_and_episode_metadata() {
        assert_eq!(
            corrected_file_name(
                "Show - S02E07 - The Beginning",
                2,
                7,
                Some(std::ffi::OsStr::new("mkv")),
            ),
            "[S02E07] Show - The Beginning.mkv"
        );
        assert_eq!(
            corrected_file_name("Show - 10", 1, 10, Some(std::ffi::OsStr::new("mkv")),),
            "[S01E10] Show.mkv"
        );
        assert_eq!(
            corrected_file_name(
                "[Uploader] Some Show 3rd [1080p AV1 10Bit][AAC][MultiSubs]",
                3,
                1,
                Some(std::ffi::OsStr::new("mkv")),
            ),
            "[S03E01] [Uploader] Some Show 3rd [1080p AV1 10Bit][AAC][MultiSubs].mkv"
        );
        assert_eq!(
            corrected_file_name(
                "[Judas] Rent-a-Girlfriend - S05E01v2",
                5,
                1,
                Some(std::ffi::OsStr::new("mkv")),
            ),
            "[S05E01] [Author] Some Show - v2.mkv"
        );
    }

    #[test]
    fn creates_hardlinks_and_skips_duplicate_episodes() {
        let root =
            std::env::temp_dir().join(format!("metadata-corrector-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let season = source.join("Season 01");
        let output = root.join("output");
        std::fs::create_dir_all(&season).unwrap();
        std::fs::write(season.join("Show - S01E01 - Pilot.mkv"), b"episode").unwrap();
        std::fs::write(season.join("Show - S01E01 - Z-duplicate.mkv"), b"duplicate").unwrap();
        std::fs::write(season.join("Show - S01E02 - Metadata.nfo"), b"metadata").unwrap();
        std::fs::write(season.join("Show - S01E02 - Episode.mkv"), b"episode 2").unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(summary.linked, 2);
        assert_eq!(summary.skipped, 2);
        let linked_file = output.join("Season 01").join("[S01E01] Show - Title.mkv");
        assert_eq!(std::fs::read(&linked_file).unwrap(), b"episode");
        assert!(
            output
                .join("Season 01")
                .join("[S01E02] Show - Episode.mkv")
                .exists()
        );
        assert!(
            !output
                .join("Season 01")
                .join("[S01E02] Show - Metadata.nfo")
                .exists()
        );
        std::fs::write(&linked_file, b"updated").unwrap();
        assert_eq!(
            std::fs::read(season.join("Show - S01E01 - Title.mkv")).unwrap(),
            b"updated"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_hardlinks_for_all_show_folders() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-all-folders-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source_root = root.join("source");
        let output_root = root.join("output");
        let first_season = source_root.join("First Show").join("Season 01");
        let second_season = source_root.join("Second Show").join("Season 02");
        std::fs::create_dir_all(&first_season).unwrap();
        std::fs::create_dir_all(&second_season).unwrap();
        std::fs::write(
            first_season.join("First Show - S01E01 - Title.mkv"),
            b"first show",
        )
        .unwrap();
        std::fs::write(
            second_season.join("Second Show - S02E03 - Return.mkv"),
            b"second show",
        )
        .unwrap();
        std::fs::create_dir_all(&output_root).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary =
            super::create_hard_links_for_all_folders(&source_root, &output_root, &mut log).unwrap();

        assert_eq!(summary.linked, 2);
        assert_eq!(summary.skipped, 0);
        assert!(
            output_root
                .join("First Show")
                .join("Season 01")
                .join("[S01E01] First Show - Title.mkv")
                .exists()
        );
        assert!(
            output_root
                .join("Second Show")
                .join("Season 02")
                .join("[S02E03] Second Show - Return.mkv")
                .exists()
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creates_hardlinks_for_standalone_movies_in_the_batch_root() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-root-movie-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source_root = root.join("source");
        let output_root = root.join("output");
        let show_season = source_root.join("Show").join("Season 01");
        std::fs::create_dir_all(&show_season).unwrap();
        std::fs::write(source_root.join("Movie.mkv"), b"movie").unwrap();
        std::fs::write(show_season.join("Show - S01E01 - Episode.mkv"), b"episode").unwrap();
        std::fs::create_dir_all(&output_root).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary =
            super::create_hard_links_for_all_folders(&source_root, &output_root, &mut log).unwrap();

        assert_eq!(summary.linked, 2);
        assert!(output_root.join("Movie.mkv").exists());
        assert!(
            output_root
                .join("Show")
                .join("Season 01")
                .join("[S01E01] Show - Episode.mkv")
                .exists()
        );
        std::fs::write(output_root.join("Movie.mkv"), b"updated").unwrap();
        assert_eq!(
            std::fs::read(source_root.join("Movie.mkv")).unwrap(),
            b"updated"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_multiple_seasons_below_nested_directories() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-nested-seasons-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let release_folder = source.join("Release S01+02+Movie");
        let first_season = release_folder.join("Season 01");
        let second_season = release_folder.join("Season 02");
        let output = root.join("output");
        std::fs::create_dir_all(&first_season).unwrap();
        std::fs::create_dir_all(&second_season).unwrap();
        std::fs::write(
            first_season.join("Show - S01E01 - Title.mkv"),
            b"first season",
        )
        .unwrap();
        std::fs::write(
            second_season.join("Show - S02E01 - Return.mkv"),
            b"second season",
        )
        .unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        assert_eq!(summary.linked, 2);
        assert_eq!(summary.skipped, 0);
        assert!(
            output
                .join("Season 01")
                .join("[S01E01] Show - Title.mkv")
                .exists()
        );
        assert!(
            output
                .join("Season 02")
                .join("[S02E01] Show - Return.mkv")
                .exists()
        );
        assert!(!output.join("Release S01+02+Movie").exists());
        let log_contents = std::fs::read_to_string(root.join("changes.log")).unwrap();
        assert!(log_contents.contains(&format!(
            "FLATTEN source={} output={} reason=nested-season-folders",
            release_folder.display(),
            output.display()
        )));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unifies_split_season_folders_using_a_shared_title() {
        let root = std::env::temp_dir().join(format!(
            "metadata-corrector-split-season-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        let output = root.join("output");
        let first_part = source.join("[Author] Show Name II [1080p][AV1][multisub][dual audio]");
        let second_part = source
            .join("[Author] Show Name II - Second Part Title S2 P2 [1080p EAC3 AV1][dual audio]");
        std::fs::create_dir_all(&first_part).unwrap();
        std::fs::create_dir_all(&second_part).unwrap();
        std::fs::write(
            first_part.join("Show Name II - S02E01 - First Part.mkv"),
            b"first",
        )
        .unwrap();
        std::fs::write(
            second_part.join("Show Name II - S02E02 - Second Part.mkv"),
            b"second",
        )
        .unwrap();
        std::fs::create_dir_all(&output).unwrap();
        let mut log = std::fs::File::create(root.join("changes.log")).unwrap();

        let summary = super::create_hard_links(&source, &output, &mut log).unwrap();

        let unified_output = output.join(first_part.file_name().unwrap());
        assert_eq!(summary.linked, 2);
        assert_eq!(summary.skipped, 0);
        assert!(
            unified_output
                .join("[S02E01] Show Name II - First Part.mkv")
                .exists()
        );
        assert!(
            unified_output
                .join("[S02E02] Show Name II - Second Part.mkv")
                .exists()
        );
        assert!(!output.join(second_part.file_name().unwrap()).exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
