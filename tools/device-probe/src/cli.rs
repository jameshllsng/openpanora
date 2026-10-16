use crate::onvif::ProbeError;
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use std::{net::IpAddr, path::PathBuf, time::Duration};
use url::Url;

#[derive(Debug, Parser)]
#[command(
    name = "device-probe",
    about = "Inspect one explicitly selected ONVIF/RTSP device",
    version
)]
pub struct Cli {
    /// Request timeout in seconds.
    #[arg(long, global = true, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub timeout: u64,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout)
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Retrieve device identity, services, media profiles, and recording capabilities.
    Inspect(DeviceArgs),
    /// Classify ONVIF media, recording search/control, replay, PTZ, and event services.
    Capabilities(DeviceArgs),
    /// Open and decode an RTSP stream with GStreamer; a TCP connection alone is not success.
    RtspTest(RtspArgs),
    /// Execute read-only ONVIF Recording Search and Replay Control diagnostics.
    Recordings(RecordingsArgs),
}

#[derive(Debug, Args)]
pub struct RecordingsArgs {
    #[command(flatten)]
    pub device: DeviceArgs,
    #[command(subcommand)]
    pub command: RecordingsCommand,
}

#[derive(Debug, Subcommand)]
pub enum RecordingsCommand {
    /// Get the device-wide recording time range and count.
    Summary,
    /// List recording configurations exposed by Recording Control.
    List,
    /// Find recordings, fetch metadata, and optionally filter by a UTC interval.
    Search(RecordingSearchArgs),
    /// Request a read-only replay URI for one recording token.
    ReplayUri(ReplayArgs),
    /// Read Replay Control configuration without changing it.
    ReplayConfiguration,
    /// Request a replay URI and require GStreamer to decode a media buffer.
    ReplayTest(ReplayTestArgs),
}

#[derive(Debug, Args)]
pub struct RecordingSearchArgs {
    /// Inclusive UTC start time (RFC 3339), used to filter discovered recording intervals.
    #[arg(long, value_parser = parse_utc)]
    pub start: Option<DateTime<Utc>>,

    /// Exclusive UTC end time (RFC 3339), used to filter discovered recording intervals.
    #[arg(long, value_parser = parse_utc)]
    pub end: Option<DateTime<Utc>>,

    /// Upper bound for recordings returned and metadata requests.
    #[arg(long, default_value_t = 32, value_parser = clap::value_parser!(u32).range(1..=128))]
    pub limit: u32,

    /// Total bounded budget for the asynchronous search lifecycle in seconds.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(2..=300))]
    pub search_timeout: u64,

    /// Maximum GetRecordingSearchResults polls before reporting an incomplete search.
    #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u16).range(1..=60))]
    pub max_polls: u16,

    /// ONVIF WaitTime sent with each GetRecordingSearchResults request, in seconds.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64).range(1..=10))]
    pub poll_wait_seconds: u64,

    /// Delay between incomplete search-result polls, in milliseconds.
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(0..=5000))]
    pub poll_interval_ms: u64,

    /// ONVIF KeepAliveTime requested for the temporary recording-search session, in seconds.
    #[arg(long, default_value_t = 45, value_parser = clap::value_parser!(u64).range(5..=300))]
    pub search_keep_alive_seconds: u64,
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Recording token returned by `recordings list` or `recordings search`.
    #[arg(long)]
    pub recording_token: String,
}

#[derive(Debug, Args)]
pub struct ReplayTestArgs {
    #[command(flatten)]
    pub replay: ReplayArgs,

    /// Requested UTC replay start (RFC 3339). The probe refuses to send a clock Range until
    /// the device's ONVIF replay semantics have been verified.
    #[arg(long, value_parser = parse_utc)]
    pub start: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Args)]
pub struct DeviceArgs {
    /// A single IPv4, IPv6, or DNS host. Schemes and paths are rejected.
    #[arg(long, global = true)]
    pub host: Option<String>,

    /// Explicit ONVIF Device Management URL. This replaces --host/--port/--https.
    #[arg(long, global = true, conflicts_with_all = ["host", "port", "https"], value_parser = parse_onvif_url)]
    pub service_url: Option<Url>,

    /// ONVIF HTTP(S) port.
    #[arg(long, global = true, default_value_t = 80)]
    pub port: u16,

    /// Use HTTPS for the ONVIF Device Management endpoint.
    #[arg(long, global = true)]
    pub https: bool,

    /// Disable TLS certificate verification for a local diagnostic only.
    #[arg(long, global = true, conflicts_with = "tls_ca_file")]
    pub insecure_tls: bool,

    /// PEM certificate authority file trusted for this diagnostic connection.
    #[arg(long, global = true, conflicts_with = "insecure_tls")]
    pub tls_ca_file: Option<PathBuf>,

    /// Explicitly allow service XAddr URLs hosted by this one alternate host.
    #[arg(long, global = true)]
    pub allow_service_host: Option<String>,

    /// ONVIF username. The password is read securely unless another source is selected.
    #[arg(long, global = true)]
    pub username: Option<String>,

    /// Read exactly one password line from standard input.
    #[arg(long, global = true, requires = "username", conflicts_with = "keyring")]
    pub password_stdin: bool,

    /// Read the password from Linux Secret Service, keyed by host and username.
    #[arg(
        long,
        global = true,
        requires = "username",
        conflicts_with = "password_stdin"
    )]
    pub keyring: bool,
}

impl DeviceArgs {
    pub fn device_service_url(&self) -> Result<Url, ProbeError> {
        let endpoint = if let Some(url) = &self.service_url {
            url.clone()
        } else {
            let host = self
                .host
                .as_deref()
                .ok_or_else(|| ProbeError::Input("--host or --service-url is required".into()))?;
            validate_host(host)?;
            let host = match host.parse::<IpAddr>() {
                Ok(IpAddr::V6(_)) => format!("[{host}]"),
                _ => host.to_owned(),
            };
            let scheme = if self.https { "https" } else { "http" };
            Url::parse(&format!(
                "{scheme}://{host}:{}/onvif/device_service",
                self.port
            ))
            .map_err(|error| ProbeError::Input(error.to_string()))?
        };
        if self.insecure_tls && endpoint.scheme() != "https" {
            return Err(ProbeError::Input(
                "--insecure-tls is only valid with an HTTPS ONVIF endpoint".into(),
            ));
        }
        Ok(endpoint)
    }

    pub fn credential_scope(&self) -> Result<String, ProbeError> {
        Ok(self
            .device_service_url()?
            .host_str()
            .ok_or_else(|| ProbeError::Input("ONVIF service URL must include a host".into()))?
            .to_owned())
    }

    pub fn allowed_service_host(&self) -> Result<Option<String>, ProbeError> {
        if let Some(host) = &self.allow_service_host {
            validate_host(host)?;
        }
        Ok(self.allow_service_host.clone())
    }
}

#[derive(Debug, Args)]
pub struct RtspArgs {
    /// Complete RTSP URL. Avoid embedding credentials.
    #[arg(long, value_parser = parse_rtsp_url)]
    pub url: Url,
}

fn validate_host(host: &str) -> Result<(), ProbeError> {
    if host.is_empty()
        || host.contains('/')
        || host.contains('@')
        || host.contains("//")
        || host.chars().any(char::is_whitespace)
    {
        return Err(ProbeError::Input(
            "--host must be one host name or IP address, without a scheme or path".into(),
        ));
    }
    Ok(())
}

fn parse_rtsp_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") {
        return Err("URL scheme must be rtsp or rtsps".into());
    }
    Ok(url)
}

fn parse_onvif_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("ONVIF service URL scheme must be http or https".into());
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err("ONVIF service URL must include a host and no user credentials".into());
    }
    Ok(url)
}

fn parse_utc(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| format!("expected an RFC 3339 UTC timestamp: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ipv6_device_endpoint() {
        let args = DeviceArgs {
            host: Some("2001:db8::10".into()),
            service_url: None,
            port: 8443,
            https: true,
            insecure_tls: false,
            tls_ca_file: None,
            allow_service_host: None,
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert_eq!(
            args.device_service_url().unwrap().as_str(),
            "https://[2001:db8::10]:8443/onvif/device_service"
        );
    }

    #[test]
    fn rejects_host_with_path() {
        let args = DeviceArgs {
            host: Some("camera.local/admin".into()),
            service_url: None,
            port: 80,
            https: false,
            insecure_tls: false,
            tls_ca_file: None,
            allow_service_host: None,
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert!(args.device_service_url().is_err());
    }

    #[test]
    fn accepts_explicit_https_service_url() {
        let args = DeviceArgs {
            host: None,
            service_url: Some(Url::parse("https://camera.test/onvif/device_service").unwrap()),
            port: 80,
            https: false,
            insecure_tls: true,
            tls_ca_file: None,
            allow_service_host: None,
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert_eq!(
            args.device_service_url().unwrap().as_str(),
            "https://camera.test/onvif/device_service"
        );
    }

    #[test]
    fn rejects_insecure_tls_for_http() {
        let args = DeviceArgs {
            host: Some("camera.test".into()),
            service_url: None,
            port: 80,
            https: false,
            insecure_tls: true,
            tls_ca_file: None,
            allow_service_host: None,
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert!(args.device_service_url().is_err());
    }

    #[test]
    fn parses_utc_as_an_absolute_time() {
        let utc = parse_utc("2026-01-02T03:04:05-03:00").unwrap();
        assert_eq!(utc.to_rfc3339(), "2026-01-02T06:04:05+00:00");
    }

    #[test]
    fn accepts_device_arguments_after_recording_subcommand() {
        let cli = Cli::try_parse_from([
            "device-probe",
            "recordings",
            "search",
            "--service-url",
            "https://camera.test/onvif/device_service",
            "--username",
            "operator",
        ])
        .unwrap();
        let Command::Recordings(args) = cli.command else {
            panic!("expected recordings command");
        };
        assert_eq!(
            args.device.device_service_url().unwrap().as_str(),
            "https://camera.test/onvif/device_service"
        );
    }
}
