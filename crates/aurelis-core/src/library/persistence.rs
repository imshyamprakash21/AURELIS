use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};

use crate::audio::AudioMetadata;

use super::{
    DiscoveryIssue, MetadataIssue, MusicLibrary, ScanIssue, ScanReport, Track, TrackId,
    scanner::scan_directory,
};

const INDEX_VERSION: u32 = 1;
const MAX_INDEX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TRACKS: usize = 2_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FileStamp {
    size: u64,
    modified_seconds: u64,
    modified_nanos: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedTrack {
    id: u64,
    path: PathBuf,
    stamp: Option<FileStamp>,
    metadata: AudioMetadata,
}

#[derive(Debug, Deserialize, Serialize)]
struct LibraryIndex {
    version: u32,
    checksum: u64,
    tracks: Vec<CachedTrack>,
}

impl FileStamp {
    fn read(path: &Path) -> io::Result<Option<Self>> {
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());

        Ok(modified.map(|modified| Self {
            size: metadata.len(),
            modified_seconds: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
        }))
    }
}

pub(super) fn scan_directory_cached(library: &mut MusicLibrary, root: &Path) -> ScanReport {
    let index_path = root.join(".aurelis_cache").join("library-index.json");
    let discovery = scan_directory(root);
    let root_failed = discovery.issues.iter().any(|issue| issue.path == root);
    let mut report = ScanReport {
        discovered: discovery.paths.len(),
        issues: discovery
            .issues
            .into_iter()
            .map(ScanIssue::Discovery)
            .collect(),
        ..ScanReport::default()
    };
    let mut incomplete_scan = !report.issues.is_empty();
    let cached_tracks = read_index(&index_path)
        .ok()
        .filter(|tracks| tracks.iter().all(|track| track.path.starts_with(root)))
        .unwrap_or_else(|| {
            library
                .tracks
                .iter()
                .filter(|track| track.path().starts_with(root))
                .map(|track| CachedTrack {
                    id: track.id().value(),
                    path: track.path().to_path_buf(),
                    stamp: None,
                    metadata: track.metadata().clone(),
                })
                .collect()
        });
    let mut cached_by_path: BTreeMap<PathBuf, CachedTrack> = cached_tracks
        .into_iter()
        .map(|track| (track.path.clone(), track))
        .collect();

    let max_id = cached_by_path
        .values()
        .map(|track| track.id)
        .max()
        .unwrap_or(0);
    let mut next_id = max_id.checked_add(1).unwrap_or(u64::MAX);
    let mut tracks = Vec::with_capacity(report.discovered);
    let mut updated_index = Vec::with_capacity(report.discovered);

    for path in discovery.paths {
        let previous = cached_by_path.remove(&path);
        let stamp = match FileStamp::read(&path) {
            Ok(stamp) => stamp,
            Err(error) => {
                incomplete_scan = true;
                report.issues.push(ScanIssue::Discovery(DiscoveryIssue {
                    path: path.clone(),
                    kind: error.kind(),
                    message: error.to_string(),
                }));
                if let Some(previous) = previous {
                    push_cached_track(&mut tracks, &mut updated_index, previous);
                }
                continue;
            }
        };

        if let Some(previous) = previous {
            if stamp.is_some() && previous.stamp == stamp {
                push_cached_track(
                    &mut tracks,
                    &mut updated_index,
                    CachedTrack {
                        path,
                        stamp,
                        ..previous
                    },
                );
                continue;
            }

            match AudioMetadata::from_file(&path) {
                Ok(metadata) => {
                    report.updated += 1;
                    push_cached_track(
                        &mut tracks,
                        &mut updated_index,
                        CachedTrack {
                            id: previous.id,
                            path,
                            stamp,
                            metadata,
                        },
                    );
                }
                Err(source) => {
                    report.failed += 1;
                    report.issues.push(ScanIssue::Metadata(MetadataIssue {
                        path: path.clone(),
                        source,
                    }));
                    push_cached_track(&mut tracks, &mut updated_index, previous);
                }
            }
        } else {
            match AudioMetadata::from_file(&path) {
                Ok(metadata) => {
                    let Some(id) = allocate_id(&mut next_id) else {
                        report.failed += 1;
                        report.issues.push(ScanIssue::Discovery(DiscoveryIssue {
                            path: path.clone(),
                            kind: io::ErrorKind::Other,
                            message: "the library exhausted its track ID space".to_owned(),
                        }));
                        continue;
                    };
                    report.added += 1;
                    push_cached_track(
                        &mut tracks,
                        &mut updated_index,
                        CachedTrack {
                            id,
                            path,
                            stamp,
                            metadata,
                        },
                    );
                }
                Err(source) => {
                    report.failed += 1;
                    report
                        .issues
                        .push(ScanIssue::Metadata(MetadataIssue { path, source }));
                }
            }
        }
    }

    // Keep cached entries from unreadable subdirectories until a complete
    // directory pass can confirm that those files were removed.
    if incomplete_scan {
        for previous in cached_by_path.into_values() {
            push_cached_track(&mut tracks, &mut updated_index, previous);
        }
    }

    tracks.sort_by(|left, right| left.path().cmp(right.path()));
    updated_index.sort_by(|left, right| left.path.cmp(&right.path));
    library.tracks = tracks;
    library.next_id = next_id;

    if !root_failed {
        let _ = save_index(&index_path, &updated_index);
    }

    report
}

fn allocate_id(next_id: &mut u64) -> Option<u64> {
    let id = *next_id;
    *next_id = id.checked_add(1)?;
    Some(id)
}

fn push_cached_track(
    tracks: &mut Vec<Track>,
    index: &mut Vec<CachedTrack>,
    mut cached: CachedTrack,
) {
    cached.metadata.normalize();
    tracks.push(Track::new(
        TrackId::new(cached.id),
        cached.path.clone(),
        cached.metadata.clone(),
    ));
    index.push(cached);
}

fn read_index(path: &Path) -> io::Result<Vec<CachedTrack>> {
    let size = fs::metadata(path)?.len();
    if size > MAX_INDEX_BYTES {
        return Err(invalid_data("library index exceeds the size limit"));
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(invalid_data("library index exceeds the size limit"));
    }

    let index: LibraryIndex = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if index.version != INDEX_VERSION || index.tracks.len() > MAX_TRACKS {
        return Err(invalid_data(
            "library index version or track count is invalid",
        ));
    }
    if tracks_checksum(&index.tracks)? != index.checksum {
        return Err(invalid_data("library index checksum is invalid"));
    }

    let mut paths = HashSet::with_capacity(index.tracks.len());
    let mut ids = HashSet::with_capacity(index.tracks.len());
    for track in &index.tracks {
        if track.id == 0 || track.id == u64::MAX || !ids.insert(track.id) {
            return Err(invalid_data("library index contains an invalid track ID"));
        }
        if !paths.insert(track.path.clone()) {
            return Err(invalid_data("library index contains a duplicate path"));
        }
    }
    Ok(index.tracks)
}

fn save_index(path: &Path, tracks: &[CachedTrack]) -> io::Result<()> {
    if tracks.len() > MAX_TRACKS {
        return Err(invalid_data("library index contains too many tracks"));
    }
    let index = LibraryIndex {
        version: INDEX_VERSION,
        checksum: tracks_checksum(tracks)?,
        tracks: tracks.to_vec(),
    };
    let bytes = serde_json::to_vec(&index)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(invalid_data("library index exceeds the size limit"));
    }

    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("library index has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_file_name(format!("library-index-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn tracks_checksum(tracks: &[CachedTrack]) -> io::Result<u64> {
    let bytes = serde_json::to_vec(tracks)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(bytes.iter().fold(0xcbf2_9ce4_8422_2325, |value, byte| {
        (value ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    }))
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use crate::{audio::AudioMetadata, library::MusicLibrary};

    use super::{CachedTrack, FileStamp, INDEX_VERSION, LibraryIndex, read_index, tracks_checksum};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-index-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).expect("create temporary test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_wav(path: &Path, sample_rate: u32, sample_count: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create WAV parent directory");
        }
        let data_len = (sample_count * 2) as u32;
        let samples = vec![0_u8; data_len as usize];
        let mut bytes = Vec::with_capacity(44 + samples.len());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&samples);
        fs::write(path, bytes).expect("write test WAV file");
    }

    #[test]
    fn index_round_trips_audio_metadata() {
        let metadata = AudioMetadata {
            title: Some("曲名".to_owned()),
            artist: Some("Artist".to_owned()),
            album: Some("Album".to_owned()),
            album_artist: Some("Album Artist".to_owned()),
            genre: Some("Genre".to_owned()),
            year: Some("2025".to_owned()),
            track: Some(4),
            disc: Some(2),
            duration: Some(Duration::new(31, 123_000_000)),
            sample_rate: Some(48_000),
            channels: Some(2),
            codec: "PCM".to_owned(),
            bit_depth: Some(24),
            bitrate: Some(1_536_000),
        };
        let tracks = vec![CachedTrack {
            id: 7,
            path: PathBuf::from("Music/Artist/曲.wav"),
            stamp: Some(FileStamp {
                size: 123,
                modified_seconds: 456,
                modified_nanos: 789,
            }),
            metadata,
        }];
        let index = LibraryIndex {
            version: INDEX_VERSION,
            checksum: tracks_checksum(&tracks).unwrap(),
            tracks,
        };

        let bytes = serde_json::to_vec(&index).unwrap();
        let decoded: LibraryIndex = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(decoded.version, INDEX_VERSION);
        assert_eq!(decoded.tracks[0].id, 7);
        assert_eq!(decoded.tracks[0].path, PathBuf::from("Music/Artist/曲.wav"));
        assert_eq!(decoded.tracks[0].metadata.title.as_deref(), Some("曲名"));
        assert_eq!(
            decoded.tracks[0].metadata.duration,
            Some(Duration::new(31, 123_000_000))
        );
        assert_eq!(tracks_checksum(&decoded.tracks).unwrap(), decoded.checksum);
    }

    #[test]
    fn cached_scan_reuses_unchanged_tracks_and_refreshes_changed_metadata() {
        let directory = TestDirectory::new();
        let path = directory.path().join("song.wav");
        write_wav(&path, 44_100, 8);

        let mut first = MusicLibrary::new();
        let initial = first.scan_directory_cached(directory.path());
        let id = first.get_by_path(&path).unwrap().id();
        assert_eq!(initial.added, 1);

        let mut unchanged = MusicLibrary::new();
        let unchanged_report = unchanged.scan_directory_cached(directory.path());
        assert_eq!(unchanged_report.added, 0);
        assert_eq!(unchanged_report.updated, 0);
        assert_eq!(unchanged.get_by_path(&path).unwrap().id(), id);

        write_wav(&path, 48_000, 16);
        let mut changed = MusicLibrary::new();
        let changed_report = changed.scan_directory_cached(directory.path());
        let track = changed.get_by_path(&path).unwrap();
        assert_eq!(changed_report.updated, 1);
        assert_eq!(track.id(), id);
        assert_eq!(track.metadata().sample_rate, Some(48_000));
    }

    #[test]
    fn cached_scan_adds_new_files_and_removes_missing_files() {
        let directory = TestDirectory::new();
        let retained_path = directory.path().join("retained.wav");
        let removed_path = directory.path().join("removed.wav");
        let new_path = directory.path().join("new.wav");
        write_wav(&retained_path, 44_100, 8);
        write_wav(&removed_path, 44_100, 8);

        let mut first = MusicLibrary::new();
        first.scan_directory_cached(directory.path());
        let retained_id = first.get_by_path(&retained_path).unwrap().id();
        let removed_id = first.get_by_path(&removed_path).unwrap().id();

        fs::remove_file(&removed_path).expect("remove test track");
        write_wav(&new_path, 48_000, 8);
        let mut refreshed = MusicLibrary::new();
        let report = refreshed.scan_directory_cached(directory.path());

        assert_eq!(report.added, 1);
        assert_eq!(refreshed.len(), 2);
        assert_eq!(
            refreshed.get_by_path(&retained_path).unwrap().id(),
            retained_id
        );
        assert!(refreshed.get_by_path(&removed_path).is_none());
        assert_ne!(refreshed.get_by_path(&new_path).unwrap().id(), removed_id);
    }

    #[test]
    fn corrupt_index_falls_back_to_scan_and_rebuilds() {
        let directory = TestDirectory::new();
        let path = directory.path().join("song.wav");
        write_wav(&path, 44_100, 8);
        let mut first = MusicLibrary::new();
        first.scan_directory_cached(directory.path());

        let index_path = directory
            .path()
            .join(".aurelis_cache")
            .join("library-index.json");
        fs::write(&index_path, b"corrupt index").expect("corrupt test index");

        let mut recovered = MusicLibrary::new();
        let report = recovered.scan_directory_cached(directory.path());

        assert_eq!(report.added, 1);
        assert_eq!(recovered.len(), 1);
        assert_eq!(read_index(&index_path).unwrap().len(), 1);
    }
}
