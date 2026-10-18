use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use tracing_subscriber::fmt::MakeWriter;

const MAX_BYTES: u64 = 2_000_000;
const KEEP_BYTES: u64 = 1_000_000;

struct Hub {
    path: PathBuf,
    write: Mutex<()>,
    console: Mutex<Option<File>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

pub fn init(path: &Path) -> anyhow::Result<()> {
    crate::paths::ensure_parent(path)?;
    OpenOptions::new().create(true).append(true).open(path)?;
    let hub = Hub {
        path: path.to_path_buf(),
        write: Mutex::new(()),
        console: Mutex::new(None),
    };
    let _ = HUB.set(hub);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(HubWriter)
        .with_max_level(tracing::Level::INFO)
        .init();
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn path() -> Option<PathBuf> {
    HUB.get().map(|hub| hub.path.clone())
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn attach_console() -> io::Result<()> {
    let hub = HUB
        .get()
        .ok_or_else(|| io::Error::other("日志还没初始化"))?;
    let console = OpenOptions::new().write(true).open("CONOUT$")?;
    *hub.console.lock().unwrap_or_else(|err| err.into_inner()) = Some(console);
    Ok(())
}

struct HubWriter;

impl<'a> MakeWriter<'a> for HubWriter {
    type Writer = HubWrite;

    fn make_writer(&'a self) -> Self::Writer {
        HubWrite
    }
}

struct HubWrite;

impl Write for HubWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(hub) = HUB.get() {
            hub.append(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Hub {
    fn append(&self, buf: &[u8]) {
        let _write = self.write.lock().unwrap_or_else(|err| err.into_inner());
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = file.write_all(buf);
            let _ = file.flush();
            if file.metadata().map(|meta| meta.len()).unwrap_or(0) > MAX_BYTES {
                drop(file);
                let _ = rotate(&self.path);
            }
        }
        if let Ok(mut console) = self.console.lock()
            && let Some(console) = console.as_mut()
        {
            let _ = console.write_all(buf);
            let _ = console.flush();
        }
    }
}

fn rotate(path: &Path) -> io::Result<()> {
    let data = fs::read(path).unwrap_or_default();
    let start = data.len().saturating_sub(KEEP_BYTES as usize);
    let start = data[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| start + index + 1)
        .unwrap_or(start);
    let mut file = File::create(path)?;
    file.write_all(&data[start..])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "awake-log-{label}-{}-{}.log",
            std::process::id(),
            crate::model::now_ms()
        ))
    }

    #[test]
    fn path_is_empty_until_init_and_append_mirrors_then_rotates() {
        assert!(path().is_none());
        let path = temp_path("mirror");
        let mirror = temp_path("console");
        let hub = Hub {
            path: path.clone(),
            write: Mutex::new(()),
            console: Mutex::new(Some(File::create(&mirror).unwrap())),
        };
        hub.append(b"hello\n");
        assert_eq!(fs::read(&path).unwrap(), b"hello\n");
        assert_eq!(fs::read(&mirror).unwrap(), b"hello\n");

        let big = temp_path("rotate");
        let mut file = File::create(&big).unwrap();
        let line = b"0123456789abcdef\n";
        let mut total = 0usize;
        while total <= MAX_BYTES as usize {
            file.write_all(line).unwrap();
            total += line.len();
        }
        drop(file);
        let original = fs::read(&big).unwrap();
        rotate(&big).unwrap();
        let kept = fs::read(&big).unwrap();
        assert!(!kept.is_empty());
        assert!(kept.len() <= KEEP_BYTES as usize);
        assert_eq!(*kept.first().unwrap(), b'0');
        assert!(original.ends_with(&kept));
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(mirror);
        let _ = fs::remove_file(big);
    }
}
