use std::{path::Path, thread, time::Duration};

use tokio::sync::watch;

mod db;
mod http;
mod live;
mod logbuf;
mod mcp;
mod model;
mod paths;
mod presence;
pub mod win;

use crate::{
    http::AppState,
    paths::{AppPaths, bind_addr},
    win::Launch,
};

pub fn run() -> anyhow::Result<()> {
    match win::launch() {
        Launch::ParentExited => Ok(()),
        Launch::Run { foreground } => start(foreground),
    }
}

fn start(foreground: bool) -> anyhow::Result<()> {
    let addr = bind_addr()?;
    let paths = AppPaths::resolve()?;
    if !win::acquire_single_instance() {
        win::open_url(&format!("http://{addr}"));
        return Ok(());
    }
    logbuf::init(&paths.log)?;
    if foreground {
        win::attach_parent();
    }
    tracing::info!(%addr, "AwakeChecker 启动");
    let db = db::Db::open(&paths.db)?;
    let live = live::Live::default();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_rx = shutdown_rx.clone();
    let sampler_rx = shutdown_rx;
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let db_for_tasks = db.clone();
    let live_for_tasks = live.clone();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = done_tx.send(Err(anyhow::anyhow!(err)));
                return;
            }
        };
        let result = runtime.block_on(async move {
            let sampler = tokio::spawn(presence::run(
                db_for_tasks.clone(),
                live_for_tasks.clone(),
                sampler_rx,
            ));
            let server = tokio::spawn(http::serve(
                addr,
                AppState {
                    db: db_for_tasks,
                    live: live_for_tasks,
                },
                server_rx,
            ));
            let sample_result = sampler.await;
            let server_result = server.await;
            if let Err(err) = &sample_result {
                tracing::error!(error = %err, "采样任务中断");
            }
            if let Ok(Err(err)) = &server_result {
                tracing::error!(error = %err, "网页服务结束");
            }
            Ok(())
        });
        let _ = done_tx.send(result);
    });

    let data_dir = paths
        .db
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    win::tray::run(&format!("http://{addr}"), &data_dir);
    let _ = shutdown_tx.send(true);
    match done_rx.recv_timeout(Duration::from_secs(8)) {
        Ok(result) => result,
        Err(_) => {
            tracing::error!("退出时后台任务没有在 8 秒内结束");
            Ok(())
        }
    }
}
