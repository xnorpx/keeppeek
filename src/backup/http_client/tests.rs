use super::{BackupClientError, BackupHttpClient, create_private_file};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const SYNTHETIC_ARCHIVE: &[u8] =
    b"PK\x03\x04synthetic configuration and private credential bytes";

pub(super) struct TestDirectory(pub(super) PathBuf);

impl TestDirectory {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("keeppeek-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn export_from_fixture(
    destination: &Path,
    declared_bytes: usize,
) -> Result<u64, BackupClientError> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut stream = accept_request(&listener);
        stream.set_read_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(FIXTURE_TIMEOUT)).unwrap();
        let mut request = BufReader::new((&stream).take(16 * 1024));
        let mut line = String::new();
        request.read_line(&mut line).unwrap();
        assert_eq!(line, "GET /config/export HTTP/1.1\r\n");
        let mut completed = false;
        for _ in 0..128 {
            line.clear();
            if request.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line == "\r\n" {
                completed = true;
                break;
            }
        }
        assert!(completed, "export request headers were incomplete");
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {declared_bytes}\r\nConnection: close\r\n\r\n"
        ).into_bytes();
        response.extend_from_slice(SYNTHETIC_ARCHIVE);
        stream.write_all(&response).unwrap();
    });
    let client = BackupHttpClient::new(&format!("http://{address}"), None).unwrap();
    let result = client.export(destination);
    server.join().unwrap();
    result
}

fn accept_request(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("export fixture accept failed: {error}"),
        }
    }
    panic!("export client did not connect to its fixture");
}

#[test]
fn failed_export_removes_partial_private_bytes() {
    let directory = TestDirectory::new();
    let destination = directory.0.join("configuration.zip");
    assert!(matches!(
        export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len() + 1),
        Err(BackupClientError::Transport)
    ));
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn export_never_overwrites_or_removes_an_existing_destination() {
    let directory = TestDirectory::new();
    let destination = directory.0.join("configuration.zip");
    fs::write(&destination, b"previous private backup").unwrap();
    assert!(export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len()).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"previous private backup");
}

#[cfg(unix)]
#[test]
fn export_preserves_owner_only_unix_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = TestDirectory::new();
    let destination = directory.0.join("configuration.zip");
    assert_eq!(
        export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len()).unwrap(),
        SYNTHETIC_ARCHIVE.len() as u64
    );
    assert_eq!(fs::read(&destination).unwrap(), SYNTHETIC_ARCHIVE);
    assert_eq!(
        fs::metadata(destination).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn private_creation_keeps_an_existing_file_intact() {
    let directory = TestDirectory::new();
    let destination = directory.0.join("configuration.zip");
    fs::write(&destination, b"existing").unwrap();
    assert!(create_private_file(&destination).is_err());
    assert_eq!(fs::read(destination).unwrap(), b"existing");
}
