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
    #[arg(long, required_unless_present = "service_url")]
    pub host: Option<String>,

    /// Explicit ONVIF Device Management URL. This replaces --host/--port/--https.
    #[arg(long, conflicts_with_all = ["host", "port", "https"], value_parser = parse_onvif_url)]
    pub service_url: Option<Url>,

    /// ONVIF HTTP(S) port.
    #[arg(long, default_value_t = 80)]
    pub port: u16,

    /// Use HTTPS for the ONVIF Device Management endpoint.
    #[arg(long)]
    pub https: bool,

    /// Disable TLS certificate verification for a local diagnostic only.
    #[arg(long)]
    pub insecure_tls: bool,

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
            username: None,
            password_stdin: false,
            keyring: false,
        };
        assert!(args.device_service_url().is_err());
    }
}
