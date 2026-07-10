use std::io::{self, Write};
use std::path::Path;

/// Write `contents` to `path` atomically via a temp file in the same directory.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn round_trips_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let data = b"{\"status\":\"complete\"}\n";
        atomic_write(&path, data).unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn no_leftover_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, b"data").unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("state.json")]);
    }

    #[test]
    fn concurrent_reader_never_sees_empty_or_partial() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let initial = serde_json::json!({"version": 0, "status": "init"});
        atomic_write(&path, serde_json::to_vec_pretty(&initial).unwrap().as_slice()).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let reader_path = path.clone();
        let reader_stop = stop.clone();

        let reader = std::thread::spawn(move || {
            let mut reads = 0u64;
            while !reader_stop.load(Ordering::Relaxed) {
                match fs::read(&reader_path) {
                    Ok(bytes) if bytes.is_empty() => {
                        panic!("read empty file at iteration {reads}");
                    }
                    Ok(bytes) => {
                        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_else(|e| {
                            panic!(
                                "unparseable JSON at iteration {reads}: {e}\ncontent: {:?}",
                                String::from_utf8_lossy(&bytes)
                            );
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => panic!("unexpected read error: {e}"),
                }
                reads += 1;
            }
            reads
        });

        for i in 1..=2000 {
            let value = serde_json::json!({"version": i, "status": "running"});
            atomic_write(&path, serde_json::to_vec_pretty(&value).unwrap().as_slice()).unwrap();
        }

        stop.store(true, Ordering::Relaxed);
        let reads = reader.join().unwrap();
        assert!(reads > 0, "reader should have completed at least one read");
    }

    #[test]
    fn failed_write_preserves_existing_contents() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("sub");
        fs::create_dir(&inner).unwrap();
        let path = inner.join("state.json");
        let original = b"{\"status\":\"original\"}\n";
        atomic_write(&path, original).unwrap();

        fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();
        let result = atomic_write(&path, b"SHOULD NOT APPEAR");
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "write to read-only dir should fail");
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "target should retain original contents after failed write"
        );
    }

    #[test]
    fn failed_first_write_leaves_no_partial_target() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("sub");
        fs::create_dir(&inner).unwrap();
        let path = inner.join("state.json");

        fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();
        let result = atomic_write(&path, b"{\"status\":\"new\"}");
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "write to read-only dir should fail");
        assert!(
            !path.exists(),
            "no partial target should exist after failed first write"
        );
    }
}
