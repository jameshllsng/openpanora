use crate::credentials::Credentials;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{SecondsFormat, Utc};
use openpanora_core::{
    Capability, CapabilityKind, CompatibilityReport, DeviceInformation, MediaProfile,
    ServiceEndpoint, SupportStatus,
};
use quick_xml::escape::escape;
use rand::RngCore;
use reqwest::StatusCode;
use roxmltree::{Document, Node};
use sha1::{Digest, Sha1};
use std::{collections::HashSet, time::Duration};
use thiserror::Error;
use url::Url;

const DEVICE_NS: &str = "http://www.onvif.org/ver10/device/wsdl";

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("invalid input: {0}")]
    Input(String),
    #[error("authentication is required or was rejected")]
    Authentication,
    #[error("request timed out")]
    Timeout,
    #[error("device does not support the requested operation: {0}")]
    Unsupported(String),
    #[error("malformed device response: {0}")]
    Malformed(String),
    #[error("network transport failed: {0}")]
    Transport(String),
    #[error("credential access failed: {0}")]
    Credential(String),
    #[error("GStreamer validation failed: {0}")]
    Media(String),
    #[error("output failed: {0}")]
    Output(String),
}

impl ProbeError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Authentication => 4,
            Self::Timeout => 5,
            Self::Unsupported(_) => 6,
            Self::Input(_) => 2,
            _ => 1,
        }
    }
}

#[async_trait]
pub trait SoapTransport: Send + Sync {
    async fn post(
        &self,
        endpoint: &Url,
        action: &str,
        envelope: String,
    ) -> Result<String, ProbeError>;
}

pub struct HttpSoapTransport {
    client: reqwest::Client,
}

impl HttpSoapTransport {
    pub fn new(timeout: Duration) -> Result<Self, ProbeError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!(
                "OpenPanora-device-probe/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|error| ProbeError::Transport(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl SoapTransport for HttpSoapTransport {
    async fn post(
        &self,
        endpoint: &Url,
        action: &str,
        envelope: String,
    ) -> Result<String, ProbeError> {
        let response = self
            .client
            .post(endpoint.clone())
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("application/soap+xml; charset=utf-8; action=\"{action}\""),
            )
            .body(envelope)
            .send()
            .await
            .map_err(classify_reqwest_error)?;

        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(ProbeError::Authentication);
        }
        let status = response.status();
        let body = response.text().await.map_err(classify_reqwest_error)?;
        if body.to_ascii_lowercase().contains("notauthorized")
            || body.to_ascii_lowercase().contains("ter:notauthorized")
        {
            return Err(ProbeError::Authentication);
        }
        if !status.is_success() {
            return Err(ProbeError::Transport(format!(
                "device returned HTTP {status}"
            )));
        }
        if let Ok(document) = Document::parse(&body)
            && document
                .descendants()
                .any(|node| node.has_tag_name("Fault"))
        {
            let reason = find_text(document.root(), "Text").unwrap_or("unspecified SOAP fault");
            return Err(ProbeError::Unsupported(reason.to_owned()));
        }
        Ok(body)
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> ProbeError {
    if error.is_timeout() {
        ProbeError::Timeout
    } else {
        ProbeError::Transport(error.to_string())
    }
}

pub struct OnvifClient<T> {
    transport: T,
    device_endpoint: Url,
    credentials: Option<Credentials>,
}

impl<T: SoapTransport> OnvifClient<T> {
    pub fn new(transport: T, device_endpoint: Url, credentials: Option<Credentials>) -> Self {
        Self {
            transport,
            device_endpoint,
            credentials,
        }
    }

    pub async fn inspect(&self) -> Result<CompatibilityReport, ProbeError> {
        let mut capabilities = Vec::new();
        let device = match self.device_information().await {
            Ok(information) => {
                capabilities.push(capability(
                    CapabilityKind::DeviceInformation,
                    SupportStatus::Supported,
                    "GetDeviceInformation returned a valid response",
                ));
                Some(information)
            }
            Err(ProbeError::Authentication) => {
                capabilities.push(capability(
                    CapabilityKind::DeviceInformation,
                    SupportStatus::AuthenticationRequired,
                    "device rejected unauthenticated or supplied credentials",
                ));
                None
            }
            Err(ProbeError::Timeout) => return Err(ProbeError::Timeout),
            Err(error) => {
                capabilities.push(capability(
                    CapabilityKind::DeviceInformation,
                    SupportStatus::Unverified,
                    format!("GetDeviceInformation could not be verified: {error}"),
                ));
                None
            }
        };

        let services = match self.services().await {
            Ok(services) => services,
            Err(ProbeError::Authentication) => {
                append_auth_required_capabilities(&mut capabilities);
                return Ok(CompatibilityReport {
                    device,
                    services: Vec::new(),
                    media_profiles: Vec::new(),
                    capabilities,
                });
            }
            Err(ProbeError::Timeout) => return Err(ProbeError::Timeout),
            Err(error) => {
                append_unverified_capabilities(&mut capabilities, &error.to_string());
                return Ok(CompatibilityReport {
                    device,
                    services: Vec::new(),
                    media_profiles: Vec::new(),
                    capabilities,
                });
            }
        };

        let namespaces: HashSet<&str> = services
            .iter()
            .map(|service| service.namespace.as_str())
            .collect();
        for (kind, needle) in [
            (CapabilityKind::RecordingSearch, "/search/wsdl"),
            (CapabilityKind::RecordingControl, "/recording/wsdl"),
            (CapabilityKind::Replay, "/replay/wsdl"),
            (CapabilityKind::Ptz, "/ptz/wsdl"),
            (CapabilityKind::Events, "/events/wsdl"),
        ] {
            let supported = namespaces
                .iter()
                .any(|namespace| namespace.contains(needle));
            capabilities.push(capability(
                kind,
                if supported {
                    SupportStatus::Supported
                } else {
                    SupportStatus::Unsupported
                },
                if supported {
                    format!("GetServices advertised {needle}")
                } else {
                    format!("GetServices did not advertise {needle}")
                },
            ));
        }

        let media_service = services.iter().find(|service| {
            service.namespace.contains("/media/wsdl") || service.namespace.contains("/media2/wsdl")
        });
        let media_profiles = if let Some(service) = media_service {
            match Url::parse(&service.endpoint) {
                Ok(endpoint) => match self.media_profiles(&endpoint, &service.namespace).await {
                    Ok(profiles) => {
                        let status = if profiles.is_empty() {
                            SupportStatus::Unverified
                        } else {
                            SupportStatus::Supported
                        };
                        capabilities.push(capability(
                            CapabilityKind::MediaProfiles,
                            status,
                            format!("GetProfiles returned {} profile(s)", profiles.len()),
                        ));
                        capabilities.push(capability(
                            CapabilityKind::LiveStreaming,
                            status,
                            "media profiles exist; an RTSP decode test is still required",
                        ));
                        profiles
                    }
                    Err(ProbeError::Authentication) => {
                        capabilities.push(capability(
                            CapabilityKind::MediaProfiles,
                            SupportStatus::AuthenticationRequired,
                            "media service requires valid credentials",
                        ));
                        capabilities.push(capability(
                            CapabilityKind::LiveStreaming,
                            SupportStatus::AuthenticationRequired,
                            "stream URI retrieval requires valid credentials",
                        ));
                        Vec::new()
                    }
                    Err(error) => {
                        capabilities.push(capability(
                            CapabilityKind::MediaProfiles,
                            SupportStatus::Unverified,
                            format!("media service was advertised but GetProfiles failed: {error}"),
                        ));
                        capabilities.push(capability(
                            CapabilityKind::LiveStreaming,
                            SupportStatus::Unverified,
                            "RTSP decode was not tested by this command",
                        ));
                        Vec::new()
                    }
                },
                Err(error) => {
                    capabilities.push(capability(
                        CapabilityKind::MediaProfiles,
                        SupportStatus::Unverified,
                        format!("device advertised an invalid media endpoint: {error}"),
                    ));
                    Vec::new()
                }
            }
        } else {
            capabilities.push(capability(
                CapabilityKind::MediaProfiles,
                SupportStatus::Unsupported,
                "GetServices did not advertise Media or Media2",
            ));
            capabilities.push(capability(
                CapabilityKind::LiveStreaming,
                SupportStatus::Unverified,
                "no ONVIF media service; a user-provided RTSP URL may still work",
            ));
            Vec::new()
        };

        Ok(CompatibilityReport {
            device,
            services,
            media_profiles,
            capabilities,
        })
    }

    async fn device_information(&self) -> Result<DeviceInformation, ProbeError> {
        let body = format!("<tds:GetDeviceInformation xmlns:tds=\"{DEVICE_NS}\"/>");
        let response = self
            .call(
                &self.device_endpoint,
                &format!("{DEVICE_NS}/GetDeviceInformation"),
                &body,
            )
            .await?;
        parse_device_information(&response)
    }

    async fn services(&self) -> Result<Vec<ServiceEndpoint>, ProbeError> {
        let body = format!(
            "<tds:GetServices xmlns:tds=\"{DEVICE_NS}\"><tds:IncludeCapability>true</tds:IncludeCapability></tds:GetServices>"
        );
        let response = self
            .call(
                &self.device_endpoint,
                &format!("{DEVICE_NS}/GetServices"),
                &body,
            )
            .await?;
        parse_services(&response)
    }

    async fn media_profiles(
        &self,
        endpoint: &Url,
        namespace: &str,
    ) -> Result<Vec<MediaProfile>, ProbeError> {
        let prefix = if namespace.contains("/media2/") {
            "tr2"
        } else {
            "trt"
        };
        let body = format!(
            "<{prefix}:GetProfiles xmlns:{prefix}=\"{}\"/>",
            escape(namespace)
        );
        let response = self
            .call(endpoint, &format!("{namespace}/GetProfiles"), &body)
            .await?;
        parse_media_profiles(&response)
    }

    async fn call(&self, endpoint: &Url, action: &str, body: &str) -> Result<String, ProbeError> {
        self.transport
            .post(
                endpoint,
                action,
                soap_envelope(body, self.credentials.as_ref()),
            )
            .await
    }
}

fn soap_envelope(body: &str, credentials: Option<&Credentials>) -> String {
    let security = credentials.map(ws_security_header).unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\">\
         <s:Header>{security}</s:Header><s:Body>{body}</s:Body></s:Envelope>"
    )
}

fn ws_security_header(credentials: &Credentials) -> String {
    let mut nonce = [0_u8; 20];
    rand::rng().fill_bytes(&mut nonce);
    let created = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut hasher = Sha1::new();
    hasher.update(nonce);
    hasher.update(created.as_bytes());
    hasher.update(credentials.password.as_bytes());
    let digest = STANDARD.encode(hasher.finalize());
    let nonce = STANDARD.encode(nonce);
    let username = escape(&credentials.username);
    format!(
        "<wsse:Security s:mustUnderstand=\"true\" \
         xmlns:wsse=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd\" \
         xmlns:wsu=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd\">\
         <wsse:UsernameToken><wsse:Username>{username}</wsse:Username>\
         <wsse:Password Type=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest\">{digest}</wsse:Password>\
         <wsse:Nonce EncodingType=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary\">{nonce}</wsse:Nonce>\
         <wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security>"
    )
}

fn parse_device_information(xml: &str) -> Result<DeviceInformation, ProbeError> {
    let document =
        Document::parse(xml).map_err(|error| ProbeError::Malformed(error.to_string()))?;
    Ok(DeviceInformation {
        manufacturer: find_text(document.root(), "Manufacturer").map(str::to_owned),
        model: find_text(document.root(), "Model").map(str::to_owned),
        firmware: find_text(document.root(), "FirmwareVersion").map(str::to_owned),
        serial_reported: find_text(document.root(), "SerialNumber").is_some_and(|v| !v.is_empty()),
        hardware_id_reported: find_text(document.root(), "HardwareId")
            .is_some_and(|v| !v.is_empty()),
    })
}

fn parse_services(xml: &str) -> Result<Vec<ServiceEndpoint>, ProbeError> {
    let document =
        Document::parse(xml).map_err(|error| ProbeError::Malformed(error.to_string()))?;
    let mut services = Vec::new();
    for service in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Service")
    {
        let Some(namespace) = child_text(service, "Namespace") else {
            continue;
        };
        let Some(endpoint) = child_text(service, "XAddr") else {
            continue;
        };
        services.push(ServiceEndpoint {
            namespace: namespace.to_owned(),
            endpoint: sanitize_endpoint(endpoint),
        });
    }
    if services.is_empty() {
        return Err(ProbeError::Malformed(
            "GetServices contained no service endpoints".into(),
        ));
    }
    Ok(services)
}

fn parse_media_profiles(xml: &str) -> Result<Vec<MediaProfile>, ProbeError> {
    let document =
        Document::parse(xml).map_err(|error| ProbeError::Malformed(error.to_string()))?;
    let profiles = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Profiles")
        .filter_map(|profile| {
            let token = profile.attribute("token")?.to_owned();
            Some(MediaProfile {
                token,
                name: child_text(profile, "Name").map(str::to_owned),
            })
        })
        .collect();
    Ok(profiles)
}

fn sanitize_endpoint(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return value.to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.to_string()
}

fn find_text<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.descendants()
        .find(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
        .and_then(|candidate| candidate.text())
}

fn child_text<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
        .and_then(|candidate| candidate.text())
}

fn capability(
    kind: CapabilityKind,
    status: SupportStatus,
    evidence: impl Into<String>,
) -> Capability {
    Capability {
        kind,
        status,
        evidence: evidence.into(),
    }
}

fn append_auth_required_capabilities(capabilities: &mut Vec<Capability>) {
    for kind in remaining_capability_kinds(capabilities) {
        capabilities.push(capability(
            kind,
            SupportStatus::AuthenticationRequired,
            "GetServices requires valid credentials",
        ));
    }
}

fn append_unverified_capabilities(capabilities: &mut Vec<Capability>, reason: &str) {
    for kind in remaining_capability_kinds(capabilities) {
        capabilities.push(capability(
            kind,
            SupportStatus::Unverified,
            format!("service discovery failed: {reason}"),
        ));
    }
}

fn remaining_capability_kinds(capabilities: &[Capability]) -> Vec<CapabilityKind> {
    let present: HashSet<_> = capabilities
        .iter()
        .map(|capability| capability.kind)
        .collect();
    [
        CapabilityKind::MediaProfiles,
        CapabilityKind::LiveStreaming,
        CapabilityKind::Ptz,
        CapabilityKind::Events,
        CapabilityKind::RecordingSearch,
        CapabilityKind::RecordingControl,
        CapabilityKind::Replay,
    ]
    .into_iter()
    .filter(|kind| !present.contains(kind))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::{collections::VecDeque, sync::Mutex};

    struct MockTransport {
        responses: Mutex<VecDeque<Result<String, ProbeError>>>,
    }

    #[async_trait]
    impl SoapTransport for MockTransport {
        async fn post(
            &self,
            _endpoint: &Url,
            _action: &str,
            _envelope: String,
        ) -> Result<String, ProbeError> {
            self.responses.lock().unwrap().pop_front().unwrap()
        }
    }

    fn client(responses: Vec<Result<&str, ProbeError>>) -> OnvifClient<MockTransport> {
        OnvifClient::new(
            MockTransport {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|response| response.map(str::to_owned))
                        .collect(),
                ),
            },
            Url::parse("http://camera.test/onvif/device_service").unwrap(),
            None,
        )
    }

    const DEVICE: &str = r#"<Envelope><Body><GetDeviceInformationResponse>
        <Manufacturer>Example</Manufacturer><Model>Cam One</Model>
        <FirmwareVersion>1.2.3</FirmwareVersion><SerialNumber>sensitive</SerialNumber>
        <HardwareId>also-sensitive</HardwareId></GetDeviceInformationResponse></Body></Envelope>"#;
    const SERVICES: &str = r#"<Envelope><Body><GetServicesResponse>
        <Service><Namespace>http://www.onvif.org/ver10/media/wsdl</Namespace>
        <XAddr>http://camera.test/onvif/media_service</XAddr></Service>
        <Service><Namespace>http://www.onvif.org/ver10/search/wsdl</Namespace>
        <XAddr>http://camera.test/onvif/search_service</XAddr></Service>
        <Service><Namespace>http://www.onvif.org/ver10/replay/wsdl</Namespace>
        <XAddr>http://camera.test/onvif/replay_service</XAddr></Service>
        </GetServicesResponse></Body></Envelope>"#;
    const PROFILES: &str = r#"<Envelope><Body><GetProfilesResponse>
        <Profiles token="main"><Name>Main stream</Name></Profiles>
        <Profiles token="sub"><Name>Sub stream</Name></Profiles>
        </GetProfilesResponse></Body></Envelope>"#;

    #[tokio::test]
    async fn discovers_recording_and_replay_without_claiming_recording_control() {
        let report = client(vec![Ok(DEVICE), Ok(SERVICES), Ok(PROFILES)])
            .inspect()
            .await
            .unwrap();
        assert_eq!(report.device.unwrap().serial_reported, true);
        assert_eq!(report.media_profiles.len(), 2);
        let status = |kind| {
            report
                .capabilities
                .iter()
                .find(|capability| capability.kind == kind)
                .unwrap()
                .status
        };
        assert_eq!(
            status(CapabilityKind::RecordingSearch),
            SupportStatus::Supported
        );
        assert_eq!(status(CapabilityKind::Replay), SupportStatus::Supported);
        assert_eq!(
            status(CapabilityKind::RecordingControl),
            SupportStatus::Unsupported
        );
    }

    #[tokio::test]
    async fn malformed_services_are_unverified() {
        let report = client(vec![Ok(DEVICE), Ok("not xml")])
            .inspect()
            .await
            .unwrap();
        assert!(report.capabilities.iter().any(|capability| {
            capability.kind == CapabilityKind::RecordingSearch
                && capability.status == SupportStatus::Unverified
        }));
    }

    #[tokio::test]
    async fn authentication_failure_is_classified() {
        let report = client(vec![
            Err(ProbeError::Authentication),
            Err(ProbeError::Authentication),
        ])
        .inspect()
        .await
        .unwrap();
        assert!(
            report
                .capabilities
                .iter()
                .all(|capability| { capability.status == SupportStatus::AuthenticationRequired })
        );
    }

    #[tokio::test]
    async fn timeout_has_a_distinct_error_and_exit_code() {
        let error = client(vec![Err(ProbeError::Timeout)])
            .inspect()
            .await
            .unwrap_err();
        assert!(matches!(error, ProbeError::Timeout));
        assert_eq!(error.exit_code(), 5);
    }

    #[test]
    fn ws_security_never_includes_plaintext_password() {
        let header = ws_security_header(&Credentials {
            username: "operator".into(),
            password: "do-not-leak".into(),
        });
        assert!(header.contains("PasswordDigest"));
        assert!(!header.contains("do-not-leak"));
    }
}
