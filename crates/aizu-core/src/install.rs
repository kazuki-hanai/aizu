//! Atomic installation of the bundled CLI into a user-managed directory.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const COPY_BUFFER_BYTES: usize = 64 * 1024;
const TEMP_CREATE_ATTEMPTS: usize = 128;

/// Result of installing a bundled CLI executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    /// No target existed and the executable was installed.
    Installed,
    /// The target already contained the same executable.
    AlreadyCurrent,
    /// An explicitly replaceable target was atomically replaced.
    Replaced,
}

/// Failures produced while validating or atomically installing the CLI.
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("source executable is not a regular file: {0}")]
    SourceNotRegular(PathBuf),
    #[error("source executable must not be a symlink: {0}")]
    SourceIsSymlink(PathBuf),
    #[error("install target must have a file name: {0}")]
    TargetHasNoFileName(PathBuf),
    #[error("install target parent is not a directory: {0}")]
    ParentNotDirectory(PathBuf),
    #[error("install target parent must not be a symlink: {0}")]
    ParentIsSymlink(PathBuf),
    #[error("install target parent is writable by another user: {0}")]
    ParentNotPrivate(PathBuf),
    #[error("install target must not be a symlink: {0}")]
    TargetIsSymlink(PathBuf),
    #[error("install target is not a regular file: {0}")]
    TargetNotRegular(PathBuf),
    #[error("install target is not owned by the target directory owner: {0}")]
    TargetOwnerMismatch(PathBuf),
    #[error("install target contains different, unmanaged content: {0}")]
    UnmanagedTarget(PathBuf),
    #[error("source and staged executable differ after copying")]
    VerificationFailed,
    #[error("install target changed while the executable was being installed: {0}")]
    TargetChanged(PathBuf),
    #[error("could not allocate a temporary install file in {0}")]
    TemporaryFileExhausted(PathBuf),
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Atomically installs `source` at `target`.
///
/// An existing target with identical bytes is treated as an idempotent install.
/// Different content is never overwritten unless `replace` is true. Symlinks,
/// non-regular files, and files not owned by the target directory owner are
/// refused even when replacement is requested.
pub fn install_cli(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
    replace: bool,
) -> Result<InstallOutcome, InstallError> {
    let source = source.as_ref();
    let target = target.as_ref();
    validate_source(source)?;

    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| InstallError::TargetHasNoFileName(target.to_path_buf()))?;
    let target_name = target
        .file_name()
        .ok_or_else(|| InstallError::TargetHasNoFileName(target.to_path_buf()))?;
    let parent_metadata = validate_parent(parent)?;

    let initial_target_state = validate_target(target, &parent_metadata)?;
    match initial_target_state {
        TargetState::Present if files_equal(source, target)? => {
            set_executable_permissions(target)?;
            File::open(target)
                .map_err(|source| io_error("open installed executable", target, source))?
                .sync_all()
                .map_err(|source| io_error("sync installed executable", target, source))?;
            sync_parent(parent)?;
            return Ok(InstallOutcome::AlreadyCurrent);
        }
        TargetState::Present if !replace => {
            return Err(InstallError::UnmanagedTarget(target.to_path_buf()));
        }
        _ => {}
    }

    let (temporary_path, mut temporary_file) = create_temporary(parent, target_name)?;
    let mut temporary = TemporaryInstall::new(temporary_path);
    let mut source_file = File::open(source)
        .map_err(|source_error| io_error("open source executable", source, source_error))?;
    copy_file(
        &mut source_file,
        source,
        &mut temporary_file,
        temporary.path(),
    )?;
    set_file_executable_permissions(&temporary_file, temporary.path())?;
    temporary_file
        .sync_all()
        .map_err(|source| io_error("sync staged executable", temporary.path(), source))?;
    drop(temporary_file);

    if !files_equal(source, temporary.path())? {
        return Err(InstallError::VerificationFailed);
    }

    // Revalidate immediately before rename. Replacing a symlink would not
    // follow it on Unix, but refusing the changed target keeps policy explicit.
    match validate_target(target, &parent_metadata)? {
        TargetState::Present if !replace => {
            return Err(InstallError::TargetChanged(target.to_path_buf()));
        }
        TargetState::Present => {
            fs::rename(temporary.path(), target)
                .map_err(|source| io_error("replace installed executable", target, source))?;
        }
        TargetState::Missing => rename_without_replacement(temporary.path(), target)?,
    }
    temporary.disarm();
    sync_parent(parent)?;

    Ok(if replace && initial_target_state == TargetState::Present {
        InstallOutcome::Replaced
    } else {
        InstallOutcome::Installed
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetState {
    Missing,
    Present,
}

fn validate_source(path: &Path) -> Result<(), InstallError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect source executable", path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(InstallError::SourceIsSymlink(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(InstallError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(())
}

fn validate_parent(path: &Path) -> Result<fs::Metadata, InstallError> {
    for ancestor in path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|source| io_error("inspect install directory ancestor", ancestor, source))?;
        if metadata.file_type().is_symlink() {
            return Err(InstallError::ParentIsSymlink(ancestor.to_path_buf()));
        }
    }

    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect install directory", path, source))?;
    if !metadata.is_dir() {
        return Err(InstallError::ParentNotDirectory(path.to_path_buf()));
    }
    #[cfg(unix)]
    if metadata.mode() & 0o022 != 0 {
        return Err(InstallError::ParentNotPrivate(path.to_path_buf()));
    }
    Ok(metadata)
}

fn validate_target(
    path: &Path,
    parent_metadata: &fs::Metadata,
) -> Result<TargetState, InstallError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(TargetState::Missing),
        Err(source) => return Err(io_error("inspect install target", path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(InstallError::TargetIsSymlink(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(InstallError::TargetNotRegular(path.to_path_buf()));
    }
    #[cfg(unix)]
    if metadata.uid() != parent_metadata.uid() {
        return Err(InstallError::TargetOwnerMismatch(path.to_path_buf()));
    }
    Ok(TargetState::Present)
}

fn create_temporary(
    parent: &Path,
    target_name: &std::ffi::OsStr,
) -> Result<(PathBuf, File), InstallError> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(target_name);
        name.push(format!(".aizu-install-{}-{sequence}", std::process::id()));
        let path = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o700);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error("create staged executable", &path, source)),
        }
    }
    Err(InstallError::TemporaryFileExhausted(parent.to_path_buf()))
}

fn copy_file(
    source: &mut File,
    source_path: &Path,
    target: &mut File,
    target_path: &Path,
) -> Result<(), InstallError> {
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|source| io_error("read source executable", source_path, source))?;
        if read == 0 {
            break;
        }
        target
            .write_all(&buffer[..read])
            .map_err(|source| io_error("write staged executable", target_path, source))?;
    }
    target
        .flush()
        .map_err(|source| io_error("flush staged executable", target_path, source))
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, InstallError> {
    let left_metadata = fs::metadata(left)
        .map_err(|source| io_error("inspect executable for verification", left, source))?;
    let right_metadata = fs::metadata(right)
        .map_err(|source| io_error("inspect executable for verification", right, source))?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }

    let mut left_file = File::open(left)
        .map_err(|source| io_error("open executable for verification", left, source))?;
    let mut right_file = File::open(right)
        .map_err(|source| io_error("open executable for verification", right, source))?;
    let mut left_buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut right_buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let left_read = left_file
            .read(&mut left_buffer)
            .map_err(|source| io_error("read executable for verification", left, source))?;
        let right_read = right_file
            .read(&mut right_buffer)
            .map_err(|source| io_error("read executable for verification", right, source))?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
fn set_file_executable_permissions(file: &File, path: &Path) -> Result<(), InstallError> {
    file.set_permissions(fs::Permissions::from_mode(0o755))
        .map_err(|source| io_error("set executable permissions", path, source))
}

#[cfg(not(unix))]
fn set_file_executable_permissions(_file: &File, _path: &Path) -> Result<(), InstallError> {
    Ok(())
}

fn set_executable_permissions(path: &Path) -> Result<(), InstallError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| io_error("open installed executable", path, source))?;
    set_file_executable_permissions(&file, path)
}

fn rename_without_replacement(source: &Path, target: &Path) -> Result<(), InstallError> {
    fs::hard_link(source, target).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            InstallError::TargetChanged(target.to_path_buf())
        } else {
            io_error("install executable", target, error)
        }
    })?;
    fs::remove_file(source)
        .map_err(|error| io_error("remove staged executable link", source, error))
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), InstallError> {
    File::open(parent)
        .map_err(|source| io_error("open install directory", parent, source))?
        .sync_all()
        .map_err(|source| io_error("sync install directory", parent, source))
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), InstallError> {
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> InstallError {
    InstallError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct TemporaryInstall {
    path: PathBuf,
    armed: bool,
}

impl TemporaryInstall {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryInstall {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn private_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(".aizu-install-test-")
            .tempdir_in(std::env::current_dir().expect("current directory"))
            .expect("tempdir")
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = private_tempdir();
        let source = directory.path().join("bundled-aizu");
        let install_directory = directory.path().join("bin");
        fs::create_dir(&install_directory).expect("install directory");
        let target = install_directory.join("aizu");
        fs::write(&source, b"bundled cli bytes").expect("source");
        (directory, source, target)
    }

    #[test]
    fn installs_and_is_idempotent() {
        let (_directory, source, target) = fixture();

        assert_eq!(
            install_cli(&source, &target, false).expect("install"),
            InstallOutcome::Installed
        );
        assert_eq!(
            fs::read(&target).expect("installed bytes"),
            b"bundled cli bytes"
        );
        assert_eq!(
            install_cli(&source, &target, false).expect("idempotent install"),
            InstallOutcome::AlreadyCurrent
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&target)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_target() {
        use std::os::unix::fs::symlink;

        let (_directory, source, target) = fixture();
        let victim = target.parent().expect("parent").join("victim");
        fs::write(&victim, b"do not replace").expect("victim");
        symlink(&victim, &target).expect("symlink");

        assert!(matches!(
            install_cli(&source, &target, true),
            Err(InstallError::TargetIsSymlink(path)) if path == target
        ));
        assert_eq!(fs::read(victim).expect("victim bytes"), b"do not replace");
    }

    #[test]
    fn refuses_unmanaged_target_without_explicit_replace() {
        let (_directory, source, target) = fixture();
        fs::write(&target, b"user binary").expect("target");

        assert!(matches!(
            install_cli(&source, &target, false),
            Err(InstallError::UnmanagedTarget(path)) if path == target
        ));
        assert_eq!(fs::read(target).expect("target bytes"), b"user binary");
    }

    #[test]
    fn explicitly_replaces_unmanaged_regular_target() {
        let (_directory, source, target) = fixture();
        fs::write(&target, b"old unmanaged binary").expect("target");

        assert_eq!(
            install_cli(&source, &target, true).expect("replace"),
            InstallOutcome::Replaced
        );
        assert_eq!(
            fs::read(target).expect("target bytes"),
            b"bundled cli bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_in_install_directory_ancestry() {
        use std::os::unix::fs::symlink;

        let directory = private_tempdir();
        let source = directory.path().join("bundled-aizu");
        fs::write(&source, b"bundled cli bytes").expect("source");
        let actual = directory.path().join("actual");
        fs::create_dir(&actual).expect("actual directory");
        let linked = directory.path().join("linked");
        symlink(&actual, &linked).expect("directory symlink");
        let nested = linked.join("bin");
        fs::create_dir(&nested).expect("nested directory");
        let target = nested.join("aizu");

        assert!(matches!(
            install_cli(&source, &target, false),
            Err(InstallError::ParentIsSymlink(path)) if path == linked
        ));
        assert!(!actual.join("bin/aizu").exists());
    }
}
