# Hardlink Creator

Hardlink Creator scans a video library and creates a second, organized view of
its files using hard links. The source media files are not copied: each
destination entry refers to the same file data as its source, so the operation
uses little additional storage. The source tree is left in place.

The destination view is intended for media servers such as Jellyfin. Episode
files are named consistently and placed in season folders while non-video
files are ignored.

## Features

- Scan one configured source folder, or batch-process each immediate
  subfolder of a source root into a matching destination folder.
- Recognize common season-folder and episode-filename conventions, including
  `Season 2`, `S02`, `S02 P2`, `S01E02`, `1x02`, and `Episode 02`, with
  episode numbers up to four digits. Release revision suffixes such as `v2`
  are removed when episode filenames are normalized.
- Group split season folders, flatten wrapper folders that contain nested
  seasons, and skip duplicate season or episode numbers deterministically.
  Unnumbered OVA/OAD, movie, and special folders retain their own destination
  folders instead of being assigned to Season 1.
- Create normalized episode names in the form
  `[SxxEyy] Title - yy.extension`, retaining the episode number in the title
  body and its original zero-padding. Direct video files with an `SxxEyy`
  marker are normalized as well; other direct video files keep their original
  name.
- Filter video files by extension (case-insensitively) and skip configured
  blacklist matches.
- Re-scan periodically, skipping existing destinations and links so repeat
  scans do not create duplicate links.
- Write scan activity to a timestamped log under `logs/` beside the
  application, and also show events in the console.

Hard links require the source and destination to be on the same filesystem.
The destination folders must also be writable by the account running the
application.

## Configuration and usage

Place `config.yaml` beside the application executable. Paths can be absolute
or relative to the executable's directory. For example:

```yaml
source_directory: "E:/media/Shows"
output_directory: "E:/hardlinks/Shows"

# false scans the single source_directory above.
# true processes each immediate child folder of source_root_directory.
process_all_folders: false
source_root_directory: "E:/media"
output_root_directory: "E:/hardlinks"

# The first scan starts immediately; later scans run at this interval.
scan_interval_seconds: 600

# Optional regular expressions checked against each path component.
# A matching folder and its contents are skipped.
blacklist:
  - '^(?i)featurettes'
```

When `process_all_folders` is `false`, configure `source_directory` and
`output_directory`. When it is `true`, configure `source_root_directory` and
`output_root_directory`; each immediate source subfolder gets a corresponding
output subfolder. Direct video files in the source root are processed there as
well. Blacklist expressions are regular expressions, are matched against
individual path components, and are compiled when the program starts.

Run the executable after configuring it. It scans immediately, then repeats
after `scan_interval_seconds` until stopped with Ctrl+C. The default interval
is 300 seconds if omitted, and zero is not allowed. Changes to configuration
take effect after restarting the application.

## TODO: Features

- Scan and create hard links for multiple libraries, with independently
  configured source and destination paths for each library.

## TODO: Optimizations

1. **Benchmark Performance** Measure full-scan time and time
   spent traversing directories, parsing names, checking destinations, and
   logging. A warm repeat scan may have different bottlenecks than one creating
   links.
2. **Cut per-file output overhead.** The logging macro writes and prints a
   line for each event, including routine skips. Buffer log writes; consider
   summarizing routine skip messages while keeping warnings and errors visible.
   Whether detailed per-file logs should remain is a user-facing tradeoff.
3. **Compile parsing regexes once.** The season and episode parsing helpers
   repeatedly construct the same regexes. Reusing compiled patterns could
   reduce CPU time when processing many folders and filenames.
4. **Avoid walking the same tree more than once.** Recursive traversal uses a
   lookahead that can enumerate nested directories again during normal
   processing. Reusing discovery results could reduce filesystem calls, but
   must preserve folder grouping and deterministic duplicate handling.
5. **Avoid rescanning destination folders for each file.** Existing-hardlink
   lookup searches the destination directory for each direct video candidate.
   A scan-local index could avoid repeated searches, with careful checks to
   preserve file-identity safety.
6. **Treat concurrency and filesystem watchers as later options.** Parallel
   work may help on some drives but hurt on others; watcher-assisted discovery
   could improve new-file latency but needs recovery from missed events and
   periodic reconciliation.