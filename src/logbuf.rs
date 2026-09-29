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

pub fn path() -> Option<PathBuf> {
    HUB.get().map(|hub| hub.path.clone())
}

pub fn attach_console() -> io::Result<()> {
    let hub = HUB.get().ok_or_else(|| io::Error::other("日志还没初始化"))?;
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
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) {
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

