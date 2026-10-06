use std::{
    fs, io,
    path::{Path, PathBuf},
};

const SUPPORTED_EXTENSIONS: [&str; 6] = ["flac", "mp3", "aac", "m4a", "ogg", "wav"];

/// The outcome of discovering supported audio file paths under a directory.
#[derive(Debug, Default)]
pub struct DiscoveryResult {
    /// Discovered file paths, sorted lexicographically for deterministic output.
    pub paths: Vec<PathBuf>,
    /// Filesystem problems encountered while scanning. Other directories are
    /// still scanned when possible.
    pub issues: Vec<DiscoveryIssue>,
}

/// A filesystem problem encountered during path discovery.
#[derive(Debug)]
pub struct DiscoveryIssue {
    /// The directory or entry associated with the problem.
    pub path: PathBuf,
    /// The kind of filesystem error.
    pub kind: io::ErrorKind,
    /// The operating system's error description.
    pub message: String,
}

/// Whether a path has one of the audio extensions supported by AURELIS.
///
/// This only checks the extension; it does not open, decode, or validate the
/// file. Extension matching is ASCII case-insensitive.
pub fn is_supported_audio_file(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

/// Recursively discover supported audio file paths under `root`.
///
/// Symbolic links are ignored, including when `root` itself is a symbolic
/// link. Filesystem errors are recorded in the result and do not stop scanning
/// other directories. Returned paths preserve the supplied root's path form;
/// paths are not canonicalized.
pub fn scan_directory(root: impl AsRef<Path>) -> DiscoveryResult {
    let root = root.as_ref();
    let mut result = DiscoveryResult::default();

    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            result.issues.push(DiscoveryIssue {
                path: root.to_path_buf(),
                kind: io::ErrorKind::InvalidInput,
                message: "symbolic-link roots are not followed".to_owned(),
            });
            return result;
        }
        Ok(metadata) if !metadata.is_dir() => {
            result.issues.push(DiscoveryIssue {
                path: root.to_path_buf(),
                kind: io::ErrorKind::InvalidInput,
                message: "scan root is not a directory".to_owned(),
            });
            return result;
        }
        Ok(_) => {}
        Err(error) => {
            result.issues.push(issue(root, error));
            return result;
        }
    }

    let mut pending_directories = vec![root.to_path_buf()];

    while let Some(directory) = pending_directories.pop() {
        let directory_entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                result.issues.push(issue(&directory, error));
                continue;
            }
        };

        let mut entries = Vec::new();
        for entry in directory_entries {
            match entry {
                Ok(entry) => entries.push(entry),
                Err(error) => result.issues.push(issue(&directory, error)),
            }
        }
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    result.issues.push(issue(&path, error));
                    continue;
                }
            };

            if file_type.is_symlink() {
                continue;
            }

            if file_type.is_dir() {
                pending_directories.push(path);
            } else if file_type.is_file() && is_supported_audio_file(&path) {
                result.paths.push(path);
            }
        }
    }

    result.paths.sort();
    result
}

fn issue(path: &Path, error: io::Error) -> DiscoveryIssue {
    DiscoveryIssue {
        path: path.to_path_buf(),
        kind: error.kind(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aurelis-scanner-{}-{sequence}", std::process::id()));
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

    fn create_file(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test directory tree");
        }
        fs::write(path, []).expect("create test file");
    }

    #[test]
    fn recognizes_each_supported_extension() {
        for extension in ["flac", "mp3", "aac", "m4a", "ogg", "wav"] {
            assert!(
                is_supported_audio_file(PathBuf::from(format!("song.{extension}"))),
                "expected .{extension} to be supported"
            );
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        for path in ["song.FLAC", "SONG.FLAC", "song.Mp3", "song.WaV"] {
            assert!(is_supported_audio_file(path), "expected {path} to match");
        }
    }

    #[test]
    fn ignores_unsupported_extensions_and_extensionless_paths() {
        for path in ["cover.jpg", "notes.txt", "folder"] {
            assert!(
                !is_supported_audio_file(path),
                "expected {path} to be ignored"
            );
        }
    }

    #[test]
    fn recursively_discovers_supported_files_and_sorts_results() {
        let directory = TestDirectory::new();
        let root = directory.path();
        create_file(&root.join("z-last.wav"));
        create_file(&root.join("nested/song.Mp3"));
        create_file(&root.join("nested/deeper/first.flac"));
        create_file(&root.join("cover.jpg"));
        create_file(&root.join("notes.txt"));

        let result = scan_directory(root);

        assert!(result.issues.is_empty());
        assert_eq!(
            result.paths,
            vec![
                root.join("nested/deeper/first.flac"),
                root.join("nested/song.Mp3"),
                root.join("z-last.wav"),
            ]
        );
    }

    #[test]
    fn empty_directory_has_no_paths_or_issues() {
        let directory = TestDirectory::new();

        let result = scan_directory(directory.path());

        assert!(result.paths.is_empty());
        assert!(result.issues.is_empty());
    }

    #[test]
    fn nonexistent_root_is_reported_without_panicking() {
        let directory = TestDirectory::new();
        let missing = directory.path().join("missing");

        let result = scan_directory(&missing);

        assert!(result.paths.is_empty());
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].path, missing);
        assert_eq!(result.issues[0].kind, io::ErrorKind::NotFound);
    }

    #[test]
    fn discovers_multiple_supported_files() {
        let directory = TestDirectory::new();
        let root = directory.path();
        for name in [
            "one.flac",
            "two.mp3",
            "three.aac",
            "four.m4a",
            "five.ogg",
            "six.wav",
        ] {
            create_file(&root.join(name));
        }

        let result = scan_directory(root);

        assert_eq!(result.paths.len(), 6);
        assert!(result.issues.is_empty());
    }

    #[test]
    fn scan_root_must_be_a_directory() {
        let directory = TestDirectory::new();
        let file = directory.path().join("song.flac");
        create_file(&file);

        let result = scan_directory(&file);

        assert!(result.paths.is_empty());
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].kind, io::ErrorKind::InvalidInput);
    }

    #[test]
    fn symbolic_links_are_not_followed_when_supported_by_the_platform() {
        let directory = TestDirectory::new();
        let root = directory.path().join("music");
        let target = directory.path().join("outside");
        fs::create_dir(&root).expect("create scan root");
        fs::create_dir(&target).expect("create symlink target");
        create_file(&target.join("outside.flac"));

        let directory_link = root.join("linked-outside");
        if let Err(error) = symlink_directory(&target, &directory_link) {
            // Creating symlinks may require extra privileges on the host.
            eprintln!("skipping symlink assertions because creation failed: {error}");
            return;
        }

        let result = scan_directory(&root);
        assert!(result.paths.is_empty());
        assert!(result.issues.is_empty());

        let root_link = directory.path().join("music-link");
        symlink_directory(&target, &root_link).expect("create root symlink after child symlink");
        let result = scan_directory(&root_link);
        assert!(result.paths.is_empty());
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].kind, io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    fn symlink_directory(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_directory(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }
}
