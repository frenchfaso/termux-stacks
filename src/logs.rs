use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_STREAM_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
    UnsafePath(PathBuf),
    TooLarge(PathBuf),
    InvalidTail(u16),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "log I/O failed: {error}"),
            Self::UnsafePath(path) => write!(formatter, "unsafe log path {}", path.display()),
            Self::TooLarge(path) => write!(
                formatter,
                "the requested log tail cannot be represented within the {}-byte per-stream limit: {}",
                MAX_STREAM_BYTES,
                path.display()
            ),
            Self::InvalidTail(tail) => {
                write!(formatter, "invalid log tail {tail}; expected 1..=200")
            }
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn tail(path: &Path, lines: u16) -> Result<Vec<String>, Error> {
    if !(1..=200).contains(&lines) {
        return Err(Error::InvalidTail(lines));
    }

    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.permissions().mode() & 0o077 != 0
    {
        return Err(Error::UnsafePath(path.to_path_buf()));
    }
    let mut file = File::open(path)?;
    let after = file.metadata()?;
    if !after.is_file() || before.dev() != after.dev() || before.ino() != after.ino() {
        return Err(Error::UnsafePath(path.to_path_buf()));
    }

    let length = after.len();
    let start = length.saturating_sub(MAX_STREAM_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    // Read only the snapshotted byte range. The workload may keep appending to
    // the file while this synchronous request is served; an unbounded
    // `read_to_end` would otherwise follow that growth beyond the response
    // limit.
    (&mut file).take(length - start).read_to_end(&mut bytes)?;

    let truncated_prefix = start != 0;
    if truncated_prefix {
        let Some(first_newline) = bytes.iter().position(|byte| *byte == b'\n') else {
            return Err(Error::TooLarge(path.to_path_buf()));
        };
        bytes.drain(..=first_newline);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }

    let all = if bytes.is_empty() {
        Vec::new()
    } else {
        bytes
            .split(|byte| *byte == b'\n')
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect::<Vec<_>>()
    };
    let requested = usize::from(lines);
    if truncated_prefix && all.len() < requested {
        return Err(Error::TooLarge(path.to_path_buf()));
    }
    Ok(all[all.len().saturating_sub(requested)..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::{Error, tail};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn returns_the_exact_bounded_tail() {
        let prefix = crate::paths::test_prefix("log-tail");
        let path = prefix.join("service.log");
        fs::write(&path, b"one\ntwo\nthree\nfour\n").expect("write log");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure log");

        assert_eq!(tail(&path, 2).expect("tail"), ["three", "four"]);
        assert_eq!(tail(&path, 200).expect("whole log").len(), 4);
        assert!(matches!(tail(&path, 0), Err(Error::InvalidTail(0))));
        fs::remove_dir_all(prefix).expect("remove prefix");
    }

    #[test]
    fn rejects_symlinks_and_oversized_single_lines() {
        let prefix = crate::paths::test_prefix("log-safety");
        let path = prefix.join("service.log");
        fs::write(&path, vec![b'x'; 70 * 1024]).expect("write large log");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure log");
        assert!(matches!(tail(&path, 1), Err(Error::TooLarge(_))));

        let link = prefix.join("link.log");
        symlink(&path, &link).expect("create log symlink");
        assert!(matches!(tail(&link, 1), Err(Error::UnsafePath(_))));
        fs::remove_dir_all(prefix).expect("remove prefix");
    }
}
