use crate::{
    Channel, CompatibilityReport, Device, ExportRequest, PlaybackRequest, RecordingQuery,
    RecordingSegment,
};
use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("authentication is required")]
    AuthenticationRequired,
    #[error("operation is not supported by this device")]
    Unsupported,
    #[error("operation timed out")]
    Timeout,
    #[error("invalid device response: {0}")]
    InvalidResponse(String),
    #[error("transport failure: {0}")]
    Transport(String),
}

pub trait DeviceRepository: Send + Sync {
    fn save(&self, device: &Device) -> Result<(), ApplicationError>;
    fn get(&self, id: Uuid) -> Result<Option<Device>, ApplicationError>;
    fn list(&self) -> Result<Vec<Device>, ApplicationError>;
    fn remove(&self, id: Uuid) -> Result<bool, ApplicationError>;
}

pub trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, ApplicationError>;
    fn set(&self, key: &str, secret: &str) -> Result<(), ApplicationError>;
    fn delete(&self, key: &str) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    async fn inspect(&self) -> Result<CompatibilityReport, ApplicationError>;
    async fn channels(&self) -> Result<Vec<Channel>, ApplicationError>;
}

#[async_trait]
pub trait LiveStreamSession: Send {
    async fn start(&mut self) -> Result<(), ApplicationError>;
    async fn stop(&mut self) -> Result<(), ApplicationError>;
}

#[async_trait]
pub trait RecordingService: Send + Sync {
    async fn search(
        &self,
        query: &RecordingQuery,
    ) -> Result<Vec<RecordingSegment>, ApplicationError>;
    async fn playback_uri(&self, request: &PlaybackRequest) -> Result<String, ApplicationError>;
    async fn export(&self, request: &ExportRequest) -> Result<(), ApplicationError>;
}
