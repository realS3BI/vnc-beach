//! Filesystem operations for a negotiated, authenticated RFB session.

use rfb::files::{self, Entry, Failure, Request, Response, CHUNK_SIZE, MAX_ENTRIES};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

type Result<T> = std::result::Result<T, Failure>;

/// An incomplete file stays temporary until its announced length is verified.
/// Dropping it removes the temporary file, including on disconnect and cancel.
pub struct Incoming {
    temp: NamedTempFile,
    destination: PathBuf,
    remaining: u64,
}

impl Incoming {
    pub fn create(destination: PathBuf, size: u64) -> io::Result<Self> {
        if destination.try_exists()? {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        let temp = tempfile::Builder::new()
            .prefix(".vnc-beach-")
            .tempfile_in(parent)?;
        Ok(Self {
            temp,
            destination,
            remaining: size,
        })
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if data.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds announced size",
            ));
        }
        self.temp.write_all(data)?;
        self.remaining -= data.len() as u64;
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        if self.remaining != 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete file"));
        }
        self.temp.as_file().sync_all()?;
        self.temp
            .persist_noclobber(&self.destination)
            .map_err(|e| e.error)?;
        Ok(())
    }
}

enum Transfer {
    Idle,
    Reading { file: File, remaining: u64 },
    Writing(Incoming),
}

pub struct Session {
    root: PathBuf,
    /// Where the viewer starts browsing, if not the system's Downloads folder.
    downloads: Option<PathBuf>,
    transfer: Transfer,
}

impl Session {
    /// Rooted at `home`, or the home directory of the user running the server.
    pub fn home(home: Option<&Path>) -> io::Result<Self> {
        let Some(home) = home else {
            return Self::new(
                dirs::home_dir()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory unavailable"))?,
            );
        };
        let mut session = Self::new(home.to_path_buf())?;
        session.downloads = Some(home.join("Downloads"));
        Ok(session)
    }

    pub fn new(root: PathBuf) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
            downloads: None,
            transfer: Transfer::Idle,
        })
    }

    pub fn hello(&self) -> Response {
        let initial = self
            .downloads
            .clone()
            .or_else(dirs::download_dir)
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .and_then(|p| p.to_str().map(|s| s.replace('\\', "/")))
            .filter(|p| files::valid_path(p))
            .unwrap_or_default();
        Response::Hello {
            root: self
                .root
                .display()
                .to_string()
                .trim_start_matches("\\\\?\\")
                .to_owned(),
            initial,
        }
    }

    /// Every failed operation discards any incomplete transfer. The connection
    /// remains usable, and the next request starts from idle.
    pub fn handle(&mut self, request: Request) -> Response {
        match self.perform(request) {
            Ok(response) => response,
            Err(failure) => {
                self.transfer = Transfer::Idle;
                Response::Failed(failure)
            }
        }
    }

    fn existing(&self, path: &str) -> Result<PathBuf> {
        if !files::valid_path(path) {
            return Err(Failure::Invalid);
        }
        let result = self.root.join(path).canonicalize().map_err(failure)?;
        if !result.starts_with(&self.root) {
            return Err(Failure::Denied);
        }
        Ok(result)
    }

    fn destination(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() || !files::valid_path(path) {
            return Err(Failure::Invalid);
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok(self.existing(parent)?.join(name))
    }

    fn perform(&mut self, request: Request) -> Result<Response> {
        match request {
            Request::Cancel => {
                self.transfer = Transfer::Idle;
                Ok(Response::Ok)
            }
            Request::List(path) => {
                self.require_idle()?;
                let dir = self.existing(&path)?;
                let mut entries = Vec::new();
                // Keep the encoded listing below the protocol frame limit too.
                let mut bytes = path.len() + 16;
                for entry in fs::read_dir(dir).map_err(failure)? {
                    let entry = entry.map_err(failure)?;
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if !files::valid_name(&name) || name.starts_with(".vnc-beach-") {
                        continue;
                    }
                    let meta = entry.path().symlink_metadata().map_err(failure)?;
                    // Symlinks and special files aren't transferable.
                    if !meta.is_dir() && !meta.is_file() {
                        continue;
                    }
                    bytes += name.len() + 11;
                    if entries.len() == MAX_ENTRIES || bytes > 900_000 {
                        return Err(Failure::TooLarge);
                    }
                    entries.push(Entry {
                        name,
                        directory: meta.is_dir(),
                        size: meta.len(),
                    });
                }
                entries.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                Ok(Response::Directory { path, entries })
            }
            Request::ReadStart(path) => {
                self.require_idle()?;
                let path = self.existing(&path)?;
                if !path.metadata().map_err(failure)?.is_file() {
                    return Err(Failure::Invalid);
                }
                let file = File::open(path).map_err(failure)?;
                let meta = file.metadata().map_err(failure)?;
                if !meta.is_file() {
                    return Err(Failure::Invalid);
                }
                let size = meta.len();
                self.transfer = Transfer::Reading {
                    file,
                    remaining: size,
                };
                Ok(Response::Started { size })
            }
            Request::ReadChunk => {
                let Transfer::Reading { file, remaining } = &mut self.transfer else {
                    return Err(Failure::Invalid);
                };
                let mut bytes = vec![0; (*remaining).min(CHUNK_SIZE as u64) as usize];
                file.read_exact(&mut bytes).map_err(failure)?;
                *remaining -= bytes.len() as u64;
                Ok(Response::Data(bytes))
            }
            Request::WriteStart { path, size } => {
                self.require_idle()?;
                let incoming = Incoming::create(self.destination(&path)?, size).map_err(failure)?;
                self.transfer = Transfer::Writing(incoming);
                Ok(Response::Ok)
            }
            Request::WriteChunk(bytes) => {
                let Transfer::Writing(incoming) = &mut self.transfer else {
                    return Err(Failure::Invalid);
                };
                incoming.write(&bytes).map_err(failure)?;
                Ok(Response::Ok)
            }
            Request::WriteFinish => {
                let Transfer::Writing(incoming) = std::mem::replace(&mut self.transfer, Transfer::Idle)
                else {
                    return Err(Failure::Invalid);
                };
                incoming.finish().map_err(failure)?;
                Ok(Response::Ok)
            }
        }
    }

    fn require_idle(&self) -> Result<()> {
        if matches!(self.transfer, Transfer::Idle) {
            Ok(())
        } else {
            Err(Failure::Busy)
        }
    }
}

fn failure(error: io::Error) -> Failure {
    log::debug!("file transfer: {error}");
    match error.kind() {
        io::ErrorKind::NotFound => Failure::NotFound,
        io::ErrorKind::PermissionDenied => Failure::Denied,
        io::ErrorKind::AlreadyExists => Failure::Exists,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            Failure::Invalid
        }
        _ => Failure::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_download_and_no_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let mut s = Session::new(root.path().into()).unwrap();
        let data: Vec<u8> = (0..CHUNK_SIZE * 2 + 7).map(|n| n as u8).collect();
        assert_eq!(
            s.handle(Request::WriteStart {
                path: "Übung.bin".into(),
                size: data.len() as u64
            }),
            Response::Ok
        );
        assert!(!root.path().join("Übung.bin").exists());
        for part in data.chunks(CHUNK_SIZE) {
            assert_eq!(s.handle(Request::WriteChunk(part.to_vec())), Response::Ok);
        }
        assert_eq!(s.handle(Request::WriteFinish), Response::Ok);
        assert_eq!(fs::read(root.path().join("Übung.bin")).unwrap(), data);
        assert_eq!(
            s.handle(Request::WriteStart {
                path: "Übung.bin".into(),
                size: 0
            }),
            Response::Failed(Failure::Exists)
        );
        assert_eq!(
            s.handle(Request::ReadStart("Übung.bin".into())),
            Response::Started {
                size: data.len() as u64
            }
        );
        let mut downloaded = Vec::new();
        while downloaded.len() < data.len() {
            let Response::Data(chunk) = s.handle(Request::ReadChunk) else {
                panic!("expected data")
            };
            downloaded.extend(chunk);
        }
        assert_eq!(downloaded, data);
        assert_eq!(s.handle(Request::Cancel), Response::Ok);
        let Response::Directory { entries, .. } = s.handle(Request::List("".into())) else {
            panic!("expected listing")
        };
        assert_eq!(entries[0].name, "Übung.bin");
    }

    #[test]
    fn cancellation_disconnect_bad_length_and_empty_files() {
        let root = tempfile::tempdir().unwrap();
        let mut s = Session::new(root.path().into()).unwrap();
        for end in [
            Request::Cancel,
            Request::WriteFinish,
            Request::WriteChunk(vec![1; 2]),
        ] {
            s.handle(Request::WriteStart {
                path: "incomplete".into(),
                size: 1,
            });
            s.handle(end);
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
        s.handle(Request::WriteStart {
            path: "disconnect".into(),
            size: 10,
        });
        drop(s);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let mut s = Session::new(root.path().into()).unwrap();
        assert_eq!(
            s.handle(Request::WriteStart {
                path: "empty".into(),
                size: 0
            }),
            Response::Ok
        );
        assert_eq!(s.handle(Request::WriteFinish), Response::Ok);
        assert_eq!(fs::metadata(root.path().join("empty")).unwrap().len(), 0);
    }

    #[test]
    fn traversal_and_concurrent_destination_creation() {
        let root = tempfile::tempdir().unwrap();
        let mut s = Session::new(root.path().into()).unwrap();
        for path in ["../escape", "/etc/passwd", "C:/Windows", "file:stream", "NUL"] {
            assert_eq!(
                s.handle(Request::WriteStart {
                    path: path.into(),
                    size: 0
                }),
                Response::Failed(Failure::Invalid)
            );
        }
        s.handle(Request::WriteStart {
            path: "race".into(),
            size: 0,
        });
        fs::write(root.path().join("race"), b"keep").unwrap();
        assert_eq!(s.handle(Request::WriteFinish), Response::Failed(Failure::Exists));
        assert_eq!(fs::read(root.path().join("race")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn cannot_follow_symlink_outside_home() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let mut s = Session::new(root.path().into()).unwrap();
        assert_eq!(
            s.handle(Request::List("link".into())),
            Response::Failed(Failure::Denied)
        );
        assert_eq!(
            s.handle(Request::WriteStart {
                path: "link/x".into(),
                size: 0
            }),
            Response::Failed(Failure::Denied)
        );
    }
}
