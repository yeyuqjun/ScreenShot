use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
}

impl FileStamp {
    pub fn read(path: &Path) -> Option<Self> {
        let metadata = path.metadata().ok()?;
        (metadata.is_file() && metadata.len() > 0).then_some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
        })
    }
}

type CheckCache = HashMap<(PathBuf, &'static str), (FileStamp, bool)>;
static CHECKS: LazyLock<Mutex<CheckCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Reuse expensive checks while a file's metadata is unchanged.
pub fn checked_file(
    path: &Path,
    check_id: &'static str,
    validate: impl FnOnce() -> bool,
) -> Option<FileStamp> {
    let stamp = FileStamp::read(path)?;
    let key = (path.to_path_buf(), check_id);
    if let Some((previous, valid)) = CHECKS.lock().unwrap().get(&key) {
        if *previous == stamp {
            return valid.then_some(stamp);
        }
    }
    let valid = validate();
    if FileStamp::read(path)? != stamp {
        return None;
    }
    let mut checks = CHECKS.lock().unwrap();
    if checks.len() >= 4096 {
        checks.clear();
    }
    checks.insert(key, (stamp, valid));
    valid.then_some(stamp)
}

pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".part");
    name.into()
}

pub fn has_local_file(path: &Path) -> bool {
    [path.to_path_buf(), part_path(path)]
        .iter()
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir()))
}

pub fn remove_model_file(path: &Path) -> io::Result<()> {
    for file in [path.to_path_buf(), part_path(path)] {
        remove_file_if_present(&file)?;
        CHECKS.lock().unwrap().retain(|(p, _), _| p != &file);
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub fn remove_empty_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir(path) {
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// Keep interrupted downloads out of the installed pack and clean up on every error path.
pub struct PendingFile(PathBuf);

impl PendingFile {
    pub fn create(dest: &Path) -> io::Result<(Self, File)> {
        let path = part_path(dest);
        remove_file_if_present(&path)?;
        let file = File::create_new(&path)?;
        Ok((Self(path), file))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn install(self, dest: &Path) -> io::Result<()> {
        fs::rename(&self.0, dest)?;
        CHECKS.lock().unwrap().retain(|(path, _), _| path != dest);
        Ok(())
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Check the ONNX ModelProto envelope without reading model weights into memory.
/// ONNX Runtime checks the graph when it opens a session.
pub fn has_onnx_envelope(path: &Path) -> bool {
    fn read_varint(file: &mut File) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let mut buf = [0];
            file.read_exact(&mut buf)?;
            let byte = buf[0];
            if shift == 63 && byte > 1 {
                break;
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(io::ErrorKind::InvalidData.into())
    }

    fn check(mut file: File) -> io::Result<bool> {
        let len = file.metadata()?.len();
        let mut has_version = false;
        let mut has_graph = false;
        while file.stream_position()? < len {
            let tag = read_varint(&mut file)?;
            let field = tag >> 3;
            if field == 0 || field >= (1 << 29) {
                return Ok(false);
            }
            let size = match tag & 7 {
                0 => {
                    let value = read_varint(&mut file)?;
                    has_version |= field == 1 && value > 0;
                    0
                }
                1 => 8,
                2 => {
                    let size = read_varint(&mut file)?;
                    has_graph |= field == 7 && size > 0;
                    size
                }
                5 => 4,
                _ => return Ok(false),
            };
            let pos = file.stream_position()?;
            if pos > len || size > len - pos {
                return Ok(false);
            }
            if size != 0 {
                file.seek(SeekFrom::Start(pos + size))?;
            }
        }
        Ok(has_version && has_graph)
    }

    File::open(path).and_then(check).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn interrupted_download_preserves_installed_file_and_repair_replaces_it() {
        let dir = std::env::temp_dir().join(format!("snapdock-model-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let dest = dir.join("model.onnx");
        fs::write(&dest, b"installed").unwrap();
        {
            let (_pending, mut file) = PendingFile::create(&dest).unwrap();
            file.write_all(b"interrupted").unwrap();
        }
        assert_eq!(fs::read(&dest).unwrap(), b"installed");
        assert!(!part_path(&dest).exists());

        let (pending, mut file) = PendingFile::create(&dest).unwrap();
        file.write_all(b"repaired").unwrap();
        drop(file);
        pending.install(&dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"repaired");
        assert!(!part_path(&dest).exists());

        remove_model_file(&dest).unwrap();
        remove_empty_dir(&dir).unwrap();
    }
}
