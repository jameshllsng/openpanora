use crate::{ApplicationError, Device, DeviceKind, DeviceRepository};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct SqliteDeviceRepository {
    connection: Mutex<Connection>,
}

impl SqliteDeviceRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let repository = Self {
            connection: Mutex::new(Connection::open(path)?),
        };
        repository.migrate()?;
        Ok(repository)
    }

    pub fn in_memory() -> Result<Self, StorageError> {
        let repository = Self {
            connection: Mutex::new(Connection::open_in_memory()?),
        };
        repository.migrate()?;
        Ok(repository)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        self.connection
            .lock()
            .expect("database mutex poisoned")
            .execute_batch(
                "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS devices (
                 id TEXT PRIMARY KEY NOT NULL,
                 name TEXT NOT NULL,
                 host TEXT NOT NULL,
                 port INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 manufacturer TEXT,
                 model TEXT,
                 firmware TEXT,
                 credential_key TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );",
            )?;
        Ok(())
    }
}

impl DeviceRepository for SqliteDeviceRepository {
    fn save(&self, device: &Device) -> Result<(), ApplicationError> {
        let kind = serde_json::to_string(&device.kind)
            .map_err(|error| ApplicationError::InvalidResponse(error.to_string()))?;
        self.connection
            .lock()
            .expect("database mutex poisoned")
            .execute(
                "INSERT INTO devices (
                    id, name, host, port, kind, manufacturer, model, firmware,
                    credential_key, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name, host = excluded.host, port = excluded.port,
                    kind = excluded.kind, manufacturer = excluded.manufacturer,
                    model = excluded.model, firmware = excluded.firmware,
                    credential_key = excluded.credential_key, updated_at = excluded.updated_at",
                params![
                    device.id.to_string(),
                    device.name,
                    device.host,
                    device.port,
                    kind,
                    device.manufacturer,
                    device.model,
                    device.firmware,
                    device.credential_key,
                    device.created_at.to_rfc3339(),
                    device.updated_at.to_rfc3339(),
                ],
            )
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        Ok(())
    }

    fn get(&self, id: Uuid) -> Result<Option<Device>, ApplicationError> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection
            .prepare(
                "SELECT id, name, host, port, kind, manufacturer, model, firmware,
                        credential_key, created_at, updated_at
                 FROM devices WHERE id = ?1",
            )
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        statement
            .query_row([id.to_string()], row_to_device)
            .optional()
            .map_err(|error| ApplicationError::Transport(error.to_string()))
    }

    fn list(&self) -> Result<Vec<Device>, ApplicationError> {
        let connection = self.connection.lock().expect("database mutex poisoned");
        let mut statement = connection
            .prepare(
                "SELECT id, name, host, port, kind, manufacturer, model, firmware,
                        credential_key, created_at, updated_at
                 FROM devices ORDER BY name COLLATE NOCASE",
            )
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        let rows = statement
            .query_map([], row_to_device)
            .map_err(|error| ApplicationError::Transport(error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApplicationError::InvalidResponse(error.to_string()))
    }

    fn remove(&self, id: Uuid) -> Result<bool, ApplicationError> {
        self.connection
            .lock()
            .expect("database mutex poisoned")
            .execute("DELETE FROM devices WHERE id = ?1", [id.to_string()])
            .map(|affected| affected > 0)
            .map_err(|error| ApplicationError::Transport(error.to_string()))
    }
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    let id: String = row.get(0)?;
    let kind: String = row.get(4)?;
    let created_at: String = row.get(9)?;
    let updated_at: String = row.get(10)?;
    Ok(Device {
        id: Uuid::parse_str(&id).map_err(conversion_error)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get::<_, u16>(3)?,
        kind: serde_json::from_str::<DeviceKind>(&kind).map_err(conversion_error)?,
        manufacturer: row.get(5)?,
        model: row.get(6)?,
        firmware: row.get(7)?,
        credential_key: row.get(8)?,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
            .map_err(conversion_error)?
            .with_timezone(&chrono::Utc),
        updated_at: chrono::DateTime::parse_from_rfc3339(&updated_at)
            .map_err(conversion_error)?
            .with_timezone(&chrono::Utc),
    })
}

fn conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_updates_lists_and_removes_devices() {
        let repository = SqliteDeviceRepository::in_memory().unwrap();
        let mut device = Device::new("Front door", "camera.example.test", 80);

        repository.save(&device).unwrap();
        assert_eq!(repository.list().unwrap(), vec![device.clone()]);

        device.name = "Front entrance".into();
        repository.save(&device).unwrap();
        assert_eq!(
            repository.get(device.id).unwrap().unwrap().name,
            "Front entrance"
        );

        assert!(repository.remove(device.id).unwrap());
        assert!(repository.list().unwrap().is_empty());
    }
}
