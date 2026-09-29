use std::{
    env, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub db: PathBuf,
    pub log: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> anyhow::Result<Self> {
        if let Ok(db) = env::var("AWAKE_DB") {
            let db = PathBuf::from(db);
            if db.as_os_str().is_empty() {
                bail!("AWAKE_DB 是空的");
            }
            let log = sibling(&db, "awake.log");
            return Ok(Self { db, log });
        }
        let dir = local_app_data()?.join("AwakeChecker");
        Ok(Self {
            db: dir.join("awake.db"),
            log: dir.join("awake.log"),
        })
    }
}

pub fn bind_addr() -> anyhow::Result<SocketAddr> {
    let raw = env::var("AWAKE_BIND").unwrap_or_else(|_| "127.0.0.1:4737".to_string());
    let addr: SocketAddr = raw
        .parse()
        .with_context(|| format!("无法解析 AWAKE_BIND={raw}"))?;
    if !addr.ip().is_loopback() {
        bail!("只允许绑定本机回环地址，当前是 {addr}");
    }
    Ok(addr)
}

fn local_app_data() -> anyhow::Result<PathBuf> {
    env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| env::current_dir())
        .context("找不到 LOCALAPPDATA")
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

pub fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
