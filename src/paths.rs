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

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn set_var(key: &str, value: Option<&str>) {
        match value {
            Some(value) => unsafe { env::set_var(key, value) },
            None => unsafe { env::remove_var(key) },
        }
    }

    fn with_env(pairs: &[(&str, Option<&str>)], check: impl FnOnce()) {
        let _guard = lock_env();
        let saved: Vec<_> = pairs
            .iter()
            .map(|(key, _)| ((*key).to_string(), env::var(key).ok()))
            .collect();
        for (key, value) in pairs {
            set_var(key, *value);
        }
        check();
        for (key, value) in saved {
            set_var(&key, value.as_deref());
        }
    }

    #[test]
    fn bind_addr_stays_on_loopback() {
        with_env(&[("AWAKE_BIND", None)], || {
            assert_eq!(bind_addr().unwrap(), "127.0.0.1:4737".parse().unwrap());
        });
        with_env(&[("AWAKE_BIND", Some("127.0.0.1:9"))], || {
            assert_eq!(bind_addr().unwrap(), "127.0.0.1:9".parse().unwrap());
        });
        with_env(&[("AWAKE_BIND", Some("[::1]:4737"))], || {
            assert_eq!(bind_addr().unwrap(), "[::1]:4737".parse().unwrap());
        });
        with_env(&[("AWAKE_BIND", Some("0.0.0.0:1"))], || {
            let err = bind_addr().unwrap_err().to_string();
            assert!(err.contains("回环"), "{err}");
        });
        with_env(&[("AWAKE_BIND", Some("not-an-address"))], || {
            let err = bind_addr().unwrap_err().to_string();
            assert!(err.contains("无法解析"), "{err}");
        });
    }

    #[test]
    fn paths_follow_awake_db_or_local_app_data() {
        with_env(&[("AWAKE_DB", Some(""))], || {
            assert!(
                AppPaths::resolve()
                    .unwrap_err()
                    .to_string()
                    .contains("空的")
            );
        });
        with_env(
            &[("AWAKE_DB", Some("/tmp/awake-checker-data/awake.db"))],
            || {
                let paths = AppPaths::resolve().unwrap();
                assert_eq!(paths.db, PathBuf::from("/tmp/awake-checker-data/awake.db"));
                assert_eq!(
                    paths.log,
                    PathBuf::from("/tmp/awake-checker-data/awake.log")
                );
            },
        );
        with_env(&[("AWAKE_DB", Some("awake.db"))], || {
            let paths = AppPaths::resolve().unwrap();
            assert_eq!(paths.db, PathBuf::from("awake.db"));
            assert_eq!(paths.log, PathBuf::from("awake.log"));
        });
        with_env(
            &[
                ("AWAKE_DB", None),
                ("LOCALAPPDATA", Some("/tmp/awake-local")),
            ],
            || {
                let paths = AppPaths::resolve().unwrap();
                assert_eq!(
                    paths.db,
                    PathBuf::from("/tmp/awake-local/AwakeChecker/awake.db")
                );
                assert_eq!(
                    paths.log,
                    PathBuf::from("/tmp/awake-local/AwakeChecker/awake.log")
                );
            },
        );
    }

    #[test]
    fn ensure_parent_creates_missing_directories() {
        let dir = env::temp_dir().join(format!(
            "awake-parent-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        let file = dir.join("nested").join("awake.db");
        ensure_parent(&file).unwrap();
        assert!(dir.join("nested").is_dir());
        ensure_parent(Path::new("awake.db")).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
