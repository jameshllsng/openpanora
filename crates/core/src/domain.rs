use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceKind {
    Camera,
    Dvr,
    Nvr,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub kind: DeviceKind,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub credential_key: Option<String>,
    pub tls_ca_file: Option<String>,
    pub insecure_tls: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Device {
    pub fn new(name: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            host: host.into(),
            port,
            kind: DeviceKind::Unknown,
            manufacturer: None,
            model: None,
            firmware: None,
            credential_key: None,
            tls_ca_file: None,
            insecure_tls: false,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub device_id: Uuid,
    pub token: String,
    pub name: String,
    pub stream_uri: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityKind {
    DeviceInformation,
    MediaProfiles,
    LiveStreaming,
    Ptz,
    Events,
    RecordingSearch,
    RecordingControl,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupportStatus {
    Supported,
    Unsupported,
    AuthenticationRequired,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub kind: CapabilityKind,
    pub status: SupportStatus,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInformation {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    /// The serial is deliberately not retained in this shared diagnostic type.
    pub serial_reported: bool,
    pub hardware_id_reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub namespace: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaProfile {
    pub token: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub device: Option<DeviceInformation>,
    pub services: Vec<ServiceEndpoint>,
    pub media_profiles: Vec<MediaProfile>,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingQuery {
    pub device_id: Uuid,
    pub channel_token: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSegment {
    pub recording_token: String,
    pub track_token: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackRequest {
    pub device_id: Uuid,
    pub recording_token: String,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Mp4,
    Matroska,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRequest {
    pub playback: PlaybackRequest,
    pub destination: String,
    pub format: ExportFormat,
}
