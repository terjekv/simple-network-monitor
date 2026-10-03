use super::*;
use crate::{
    domain::tcp::{CheckId, TcpSnapshot},
    storage::TcpRepository,
};

type Snapshots = BTreeMap<CheckId, TcpSnapshot>;

#[async_trait]
impl TcpRepository for SqliteStorage {
    async fn update_tcp(
        &self,
        host_id: &str,
        check_id: &CheckId,
        snapshot: TcpSnapshot,
    ) -> Result<(), StorageError> {
        let host = self
            .hosts
            .read()
            .map_err(|_| StorageError::LockPoisoned)?
            .get(host_id)
            .cloned()
            .ok_or_else(|| StorageError::NotFound(host_id.into()))?;
        let check = host
            .modules
            .tcp
            .checks
            .iter()
            .find(|check| check.id == *check_id && host.modules.tcp.enabled)
            .ok_or_else(|| StorageError::InvalidData("TCP check is absent or disabled".into()))?;
        let port = check.port.get();
        let check_id = check_id.as_str().to_owned();
        self.with_inner(move |inner| {
            inner.conn.execute("INSERT INTO latest_tcp VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(host_id, check_id) DO UPDATE SET address=excluded.address, port=excluded.port, snapshot=excluded.snapshot", params![host.id, check_id, host.address, port, serde_json::to_string(&snapshot)?])?;
            Ok(())
        }).await
    }
}

pub(super) fn sync_checks(conn: &Connection, hosts: &[Host]) -> Result<(), StorageError> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS incoming_tcp (host_id TEXT, check_id TEXT, address TEXT, port INTEGER, PRIMARY KEY(host_id, check_id)); DELETE FROM incoming_tcp;")?;
    for host in hosts.iter().filter(|host| host.modules.tcp.enabled) {
        for check in &host.modules.tcp.checks {
            conn.execute(
                "INSERT INTO incoming_tcp VALUES (?1, ?2, ?3, ?4)",
                params![host.id, check.id.as_str(), host.address, check.port.get()],
            )?;
        }
    }
    conn.execute("DELETE FROM latest_tcp WHERE NOT EXISTS (SELECT 1 FROM incoming_tcp i WHERE i.host_id=latest_tcp.host_id AND i.check_id=latest_tcp.check_id AND i.address=latest_tcp.address AND i.port=latest_tcp.port)", [])?;
    Ok(())
}

pub(super) fn load_host(conn: &Connection, host: &Host) -> Result<Snapshots, StorageError> {
    let hosts = BTreeMap::from([(host.id.clone(), host.clone())]);
    Ok(load(conn, &hosts, Some(&host.id))?
        .remove(&host.id)
        .unwrap_or_default())
}

pub(super) fn load_all(
    conn: &Connection,
    hosts: &BTreeMap<String, Host>,
) -> Result<BTreeMap<String, Snapshots>, StorageError> {
    load(conn, hosts, None)
}

fn load(
    conn: &Connection,
    hosts: &BTreeMap<String, Host>,
    id: Option<&str>,
) -> Result<BTreeMap<String, Snapshots>, StorageError> {
    let sql = if id.is_some() {
        "SELECT host_id, check_id, address, port, snapshot FROM latest_tcp WHERE host_id = ?1"
    } else {
        "SELECT host_id, check_id, address, port, snapshot FROM latest_tcp"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(id))?;
    let mut result = BTreeMap::<String, Snapshots>::new();
    while let Some(row) = rows.next()? {
        let host_id: String = row.get(0)?;
        let check_id: String = row.get(1)?;
        let address: String = row.get(2)?;
        let port: u16 = row.get(3)?;
        let Some(host) = hosts.get(&host_id) else {
            continue;
        };
        // A read using an older catalog must not attach a changed target's result.
        if host.address != address || !host.modules.tcp.enabled {
            continue;
        }
        let Some(check) = host
            .modules
            .tcp
            .checks
            .iter()
            .find(|check| check.id.as_str() == check_id && check.port.get() == port)
        else {
            continue;
        };
        let json: String = row.get(4)?;
        result
            .entry(host_id)
            .or_default()
            .insert(check.id.clone(), serde_json::from_str(&json)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
