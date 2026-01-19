use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::info;
use uuid::Uuid;

use crate::error::{AppError, Result};

/// Instance state in the metadata store
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    Active,
    Archived,
    Restoring,
}

impl InstanceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Restoring => "restoring",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            "restoring" => Some(Self::Restoring),
            _ => None,
        }
    }
}

/// Instance data stored in SQLite
#[derive(Debug, Clone)]
pub struct StoredInstance {
    pub db_id: Uuid,
    pub dialect: String,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub status: InstanceState,
    pub container_id: Option<String>,
    pub host_port: Option<u16>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub backup_key: Option<String>,
    pub backup_size_bytes: Option<i64>,
}

/// SQLite-backed metadata store for instance tracking
pub struct MetadataStore {
    conn: Arc<Mutex<Connection>>,
}

impl MetadataStore {
    /// Create a new metadata store, initializing the database if needed
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.as_ref().parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::Storage(format!("Failed to create metadata directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| AppError::Storage(format!("Failed to open metadata database: {}", e)))?;

        // Initialize schema
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS instances (
                db_id TEXT PRIMARY KEY,
                dialect TEXT NOT NULL,
                db_name TEXT NOT NULL,
                db_user TEXT NOT NULL,
                db_password TEXT NOT NULL,
                status TEXT NOT NULL,
                container_id TEXT,
                host_port INTEGER,
                created_at TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                archived_at TEXT,
                backup_key TEXT,
                backup_size_bytes INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_instances_status ON instances(status);
            CREATE INDEX IF NOT EXISTS idx_instances_last_activity ON instances(last_activity);
            "#,
        )
        .map_err(|e| AppError::Storage(format!("Failed to initialize schema: {}", e)))?;

        info!("Metadata store initialized");

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert a new instance
    pub fn insert_instance(&self, instance: &StoredInstance) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO instances (
                db_id, dialect, db_name, db_user, db_password, status,
                container_id, host_port, created_at, last_activity,
                archived_at, backup_key, backup_size_bytes
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            params![
                instance.db_id.to_string(),
                instance.dialect,
                instance.db_name,
                instance.db_user,
                instance.db_password,
                instance.status.as_str(),
                instance.container_id,
                instance.host_port,
                instance.created_at.to_rfc3339(),
                instance.last_activity.to_rfc3339(),
                instance.archived_at.map(|dt| dt.to_rfc3339()),
                instance.backup_key,
                instance.backup_size_bytes,
            ],
        )
        .map_err(|e| AppError::Storage(format!("Failed to insert instance: {}", e)))?;

        Ok(())
    }

    /// Get an instance by ID
    pub fn get_instance(&self, db_id: Uuid) -> Result<Option<StoredInstance>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
            SELECT db_id, dialect, db_name, db_user, db_password, status,
                   container_id, host_port, created_at, last_activity,
                   archived_at, backup_key, backup_size_bytes
            FROM instances WHERE db_id = ?1
            "#,
            )
            .map_err(|e| AppError::Storage(format!("Failed to prepare query: {}", e)))?;

        let result = stmt
            .query_row(params![db_id.to_string()], |row| {
                Ok(Self::row_to_instance(row)?)
            })
            .optional()
            .map_err(|e| AppError::Storage(format!("Failed to query instance: {}", e)))?;

        Ok(result)
    }

    /// Update an instance
    pub fn update_instance(&self, instance: &StoredInstance) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE instances SET
                dialect = ?2, db_name = ?3, db_user = ?4, db_password = ?5,
                status = ?6, container_id = ?7, host_port = ?8,
                created_at = ?9, last_activity = ?10, archived_at = ?11,
                backup_key = ?12, backup_size_bytes = ?13
            WHERE db_id = ?1
            "#,
            params![
                instance.db_id.to_string(),
                instance.dialect,
                instance.db_name,
                instance.db_user,
                instance.db_password,
                instance.status.as_str(),
                instance.container_id,
                instance.host_port,
                instance.created_at.to_rfc3339(),
                instance.last_activity.to_rfc3339(),
                instance.archived_at.map(|dt| dt.to_rfc3339()),
                instance.backup_key,
                instance.backup_size_bytes,
            ],
        )
        .map_err(|e| AppError::Storage(format!("Failed to update instance: {}", e)))?;

        Ok(())
    }

    /// Mark an instance as archived with backup info
    pub fn mark_archived(&self, db_id: Uuid, backup_key: &str, size: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE instances SET
                status = 'archived',
                container_id = NULL,
                host_port = NULL,
                archived_at = ?2,
                backup_key = ?3,
                backup_size_bytes = ?4
            WHERE db_id = ?1
            "#,
            params![db_id.to_string(), now, backup_key, size],
        )
        .map_err(|e| AppError::Storage(format!("Failed to mark archived: {}", e)))?;

        Ok(())
    }

    /// Mark an instance as active with container info
    pub fn mark_active(&self, db_id: Uuid, container_id: &str, port: u16) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            r#"
            UPDATE instances SET
                status = 'active',
                container_id = ?2,
                host_port = ?3,
                last_activity = ?4,
                archived_at = NULL
            WHERE db_id = ?1
            "#,
            params![db_id.to_string(), container_id, port, now],
        )
        .map_err(|e| AppError::Storage(format!("Failed to mark active: {}", e)))?;

        Ok(())
    }

    /// Update status only
    pub fn update_status(&self, db_id: Uuid, status: InstanceState) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE instances SET status = ?2 WHERE db_id = ?1",
            params![db_id.to_string(), status.as_str()],
        )
        .map_err(|e| AppError::Storage(format!("Failed to update status: {}", e)))?;

        Ok(())
    }

    /// Update last activity timestamp
    pub fn touch_activity(&self, db_id: Uuid) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE instances SET last_activity = ?2 WHERE db_id = ?1",
            params![db_id.to_string(), now],
        )
        .map_err(|e| AppError::Storage(format!("Failed to touch activity: {}", e)))?;

        Ok(())
    }

    /// List all active instances
    pub fn list_active_instances(&self) -> Result<Vec<StoredInstance>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                r#"
            SELECT db_id, dialect, db_name, db_user, db_password, status,
                   container_id, host_port, created_at, last_activity,
                   archived_at, backup_key, backup_size_bytes
            FROM instances WHERE status = 'active'
            "#,
            )
            .map_err(|e| AppError::Storage(format!("Failed to prepare query: {}", e)))?;

        let instances = stmt
            .query_map([], |row| Ok(Self::row_to_instance(row)?))
            .map_err(|e| AppError::Storage(format!("Failed to query instances: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| AppError::Storage(format!("Failed to collect instances: {}", e)))?;

        Ok(instances)
    }

    /// Get instances that have been inactive longer than the timeout
    pub fn get_expired_instances(&self, timeout: Duration) -> Result<Vec<StoredInstance>> {
        let conn = self.conn.lock().unwrap();
        let cutoff = (Utc::now() - chrono::Duration::from_std(timeout).unwrap()).to_rfc3339();

        let mut stmt = conn
            .prepare(
                r#"
            SELECT db_id, dialect, db_name, db_user, db_password, status,
                   container_id, host_port, created_at, last_activity,
                   archived_at, backup_key, backup_size_bytes
            FROM instances
            WHERE status = 'active' AND last_activity < ?1
            "#,
            )
            .map_err(|e| AppError::Storage(format!("Failed to prepare query: {}", e)))?;

        let instances = stmt
            .query_map(params![cutoff], |row| Ok(Self::row_to_instance(row)?))
            .map_err(|e| AppError::Storage(format!("Failed to query expired: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| AppError::Storage(format!("Failed to collect expired: {}", e)))?;

        Ok(instances)
    }

    /// Delete an instance from the metadata store
    pub fn delete_instance(&self, db_id: Uuid) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM instances WHERE db_id = ?1",
            params![db_id.to_string()],
        )
        .map_err(|e| AppError::Storage(format!("Failed to delete instance: {}", e)))?;

        Ok(())
    }

    fn row_to_instance(row: &rusqlite::Row) -> rusqlite::Result<StoredInstance> {
        let db_id_str: String = row.get(0)?;
        let status_str: String = row.get(5)?;
        let created_at_str: String = row.get(8)?;
        let last_activity_str: String = row.get(9)?;
        let archived_at_str: Option<String> = row.get(10)?;

        Ok(StoredInstance {
            db_id: Uuid::parse_str(&db_id_str).unwrap(),
            dialect: row.get(1)?,
            db_name: row.get(2)?,
            db_user: row.get(3)?,
            db_password: row.get(4)?,
            status: InstanceState::from_str(&status_str).unwrap_or(InstanceState::Active),
            container_id: row.get(6)?,
            host_port: row.get(7)?,
            created_at: DateTime::parse_from_rfc3339(&created_at_str)
                .unwrap()
                .with_timezone(&Utc),
            last_activity: DateTime::parse_from_rfc3339(&last_activity_str)
                .unwrap()
                .with_timezone(&Utc),
            archived_at: archived_at_str.map(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .unwrap()
                    .with_timezone(&Utc)
            }),
            backup_key: row.get(11)?,
            backup_size_bytes: row.get(12)?,
        })
    }
}
