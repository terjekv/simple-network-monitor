//! A supervised scheduler independent of monitor reloads and probe execution.
use crate::storage::{SqliteStorage, StorageError};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

pub async fn run(
    storage: Arc<SqliteStorage>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), StorageError> {
    storage.recover_maintenance().await?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _=shutdown.changed()=>return Ok(()),
            _=tick.tick()=> {
                // Finish any admitted blocking database work before acknowledging shutdown.
                if let Err(err)=storage.run_due_maintenance().await { tracing::error!(%err,"maintenance scheduler failed"); }
            }
        }
    }
}
