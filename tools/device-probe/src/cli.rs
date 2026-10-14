use crate::onvif::ProbeError;
use clap::{Args, Parser, Subcommand};
use std::{net::IpAddr, time::Duration};
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
}

#[derive(Debug, Clone, Args)]
pub struct DeviceArgs {
    /// A single IPv4, IPv6, or DNS host. Schemes and paths are rejected.
    #[arg(long)]
    pub host: String,

    /// ONVIF HTTP(S) port.
    #[arg(long, default_value_t = 80)]
    pub port: u16,

    /// Use HTTPS for the ONVIF Device Management endpoint.
    #[arg(long)]
    pub https: bool,

    /// ONVIF username. The password is read securely unless another source is selected.
    #[arg(long)]
    pub username: Option<String>,

    /// Read exactly one password line from standard input.
    #[arg(long, requires = "username", conflicts_with = "keyring")]
    pub password_stdin: bool,

    /// Read the password from Linux Secret Service, keyed by host and username.
    #[arg(long, requires = "username", conflicts_with = "password_stdin")]
    pub keyring: bool,
}

impl DeviceArgs {
    pub fn device_service_url(&self) -> Result<Url, ProbeError> {
        validate_host(&self.host)?;
        let host = match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V6(_)) => format!("[{}]", self.host),
            _ => self.host.clone(),
        };
        let scheme = if self.https { "https" } else { "http" };
        Url::parse(&format!(
            "{scheme}://{host}:{}/onvif/device_service",
            self.port
        ))
        .map_err(|error| ProbeError::Input(error.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ipv6_device_endpoint() {
        let args = DeviceArgs {
            host: "2001:db8::10".into(),
            port: 8443,
            https: true,
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
            host: "camera.local/admin".into(),
            port: 80,
            https: false,
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert!(args.device_service_url().is_err());
    }
}
