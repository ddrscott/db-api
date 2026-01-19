use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceStatus {
    Starting,
    Running,
    Stopped,
    Destroyed,
}

#[derive(Debug, Clone)]
pub struct DbInstance {
    pub id: Uuid,
    pub dialect: String,
    pub container_id: String,
    pub host_port: u16,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub status: InstanceStatus,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

impl DbInstance {
    pub fn new(
        id: Uuid,
        dialect: String,
        container_id: String,
        host_port: u16,
        db_name: String,
        db_user: String,
        db_password: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            dialect,
            container_id,
            host_port,
            db_name,
            db_user,
            db_password,
            status: InstanceStatus::Starting,
            created_at: now,
            last_activity: now,
        }
    }

    pub fn touch(&mut self) {
        self.last_activity = Utc::now();
    }
}
