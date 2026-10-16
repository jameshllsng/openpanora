use crate::credentials::Credentials;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{SecondsFormat, Utc};
use md5::Md5;
use openpanora_core::{
    Capability, CapabilityKind, CompatibilityReport, DeviceInformation, MediaProfile,
    ServiceEndpoint, SupportStatus,
};
use quick_xml::escape::escape;
use rand::RngCore;
use reqwest::{StatusCode, header};
use roxmltree::{Document, Node};
use sha1::{Digest, Sha1};
use std::{collections::HashSet, error::Error as StdError, fmt::Write, path::Path, time::Duration};
use thiserror::Error;
use url::Url;

const DEVICE_NS: &str = "http://www.onvif.org/ver10/device/wsdl";

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("invalid input: {0}")]
    Input(String),
    #[error("authentication is required or was rejected")]
    Authentication,
    #[error("HTTP Digest credentials were rejected")]
    InvalidCredentials,
    #[error("HTTP 401 Unauthorized")]
    HttpUnauthorized,
    #[error("HTTP 403 Forbidden")]
    HttpForbidden,
    #[error("device requires HTTPS transport (SOAP ter:TransModeError)")]
    TransportMode,
    #[error("TLS certificate validation failed: {0}")]
    Tls(String),
    #[error("request timed out")]
    Timeout,
    #[error("{operation} timed out after {elapsed_ms} ms")]
    OperationTimeout { operation: String, elapsed_ms: u128 },
    #[error("operation was cancelled")]
    Cancelled,
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
            Self::Authentication | Self::InvalidCredentials | Self::HttpUnauthorized => 4,
            Self::Timeout | Self::OperationTimeout { .. } => 5,
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
    credentials: Option<Credentials>,
}

impl HttpSoapTransport {
    pub fn new(
        timeout: Duration,
        credentials: Option<Credentials>,
        insecure_tls: bool,
        tls_ca_file: Option<&Path>,
    ) -> Result<Self, ProbeError> {
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(insecure_tls)
            .user_agent(concat!(
                "OpenPanora-device-probe/",
                env!("CARGO_PKG_VERSION")
            ));
        if let Some(path) = tls_ca_file {
            let pem = std::fs::read(path).map_err(|error| {
                ProbeError::Input(format!(
                    "could not read --tls-ca-file {}: {error}",
                    path.display()
                ))
            })?;
            let certificate = reqwest::Certificate::from_pem(&pem).map_err(|error| {
                ProbeError::Input(format!("--tls-ca-file is not a PEM certificate: {error}"))
            })?;
            builder = builder.add_root_certificate(certificate);
        }
        let client = builder
            .build()
            .map_err(|error| ProbeError::Transport(error.to_string()))?;
        Ok(Self {
            client,
            credentials,
        })
    }

    async fn send(
        &self,
        endpoint: &Url,
        action: &str,
        envelope: String,
        authorization: Option<String>,
    ) -> Result<reqwest::Response, ProbeError> {
        let mut request = self.client.post(endpoint.clone()).header(
            header::CONTENT_TYPE,
            format!("application/soap+xml; charset=utf-8; action=\"{action}\""),
        );
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        request
            .body(envelope)
            .send()
            .await
            .map_err(classify_reqwest_error)
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
        let response = self.send(endpoint, action, envelope.clone(), None).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let Some(credentials) = &self.credentials else {
                return Err(ProbeError::HttpUnauthorized);
            };
            let challenge = digest_challenge(response.headers())?;
            // Consume the challenge response before retrying. Some cameras keep the TCP
            // connection alive and reject a follow-up request if its previous response body
            // was not read completely.
            let _ = response.bytes().await.map_err(classify_reqwest_error)?;
            let authorization = digest_authorization(credentials, endpoint, &challenge, "POST", 1)?;
            let retry = self
                .send(endpoint, action, envelope, Some(authorization))
                .await?;
            return classify_response(retry, true).await;
        }
        classify_response(response, false).await
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> ProbeError {
    if error.is_timeout() {
        ProbeError::Timeout
    } else {
        let detail = safe_transport_detail(&error);
        let classified = classify_transport_message(&detail);
        match classified {
            ProbeError::Transport(_) if error.is_connect() => {
                ProbeError::Transport(format!("connection failed: {detail}"))
            }
            ProbeError::Transport(_) if error.is_request() => {
                ProbeError::Transport(format!("request failed: {detail}"))
            }
            ProbeError::Transport(_) if error.is_body() => {
                ProbeError::Transport(format!("response body transfer failed: {detail}"))
            }
            other => other,
        }
    }
}

fn safe_transport_detail(error: &reqwest::Error) -> String {
    let mut source = error.source();
    let mut detail = None;
    while let Some(cause) = source {
        detail = Some(cause.to_string());
        source = cause.source();
    }
    let detail = detail.unwrap_or_else(|| "request could not be sent".into());
    // A source-chain detail is intentionally used instead of reqwest's display string,
    // which can include a full URL. Bound it so a peer cannot cause oversized diagnostics.
    detail.chars().take(240).collect()
}

fn classify_transport_message(message: &str) -> ProbeError {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("certificate")
        || normalized.contains("tls")
        || normalized.contains("ssl")
        || normalized.contains("rustls")
    {
        ProbeError::Tls("certificate or TLS handshake validation failed".into())
    } else {
        ProbeError::Transport(message.into())
    }
}

async fn classify_response(
    response: reqwest::Response,
    was_digest_attempt: bool,
) -> Result<String, ProbeError> {
    let status = response.status();
    let body = response.text().await.map_err(classify_reqwest_error)?;
    if let Some(error) = soap_fault_error(&body) {
        return Err(error);
    }
    match status {
        StatusCode::UNAUTHORIZED if was_digest_attempt => Err(ProbeError::InvalidCredentials),
        StatusCode::UNAUTHORIZED => Err(ProbeError::HttpUnauthorized),
        StatusCode::FORBIDDEN => Err(ProbeError::HttpForbidden),
        status if !status.is_success() => Err(ProbeError::Transport(format!(
            "device returned HTTP {status}"
        ))),
        _ => Ok(body),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: String,
}

fn digest_challenge(headers: &header::HeaderMap) -> Result<DigestChallenge, ProbeError> {
    let value = headers
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("digest ")
        })
        .ok_or_else(|| {
            ProbeError::Unsupported("HTTP 401 did not advertise HTTP Digest authentication".into())
        })?;
    let parameters = parse_digest_parameters(value.trim_start()[6..].trim());
    let realm = parameters
        .get("realm")
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| ProbeError::Malformed("Digest challenge omitted realm".into()))?;
    let nonce = parameters
        .get("nonce")
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| ProbeError::Malformed("Digest challenge omitted nonce".into()))?;
    let qop_auth = parameters
        .get("qop")
        .is_some_and(|value| value.split(',').any(|item| item.trim() == "auth"));
    if !qop_auth {
        return Err(ProbeError::Unsupported(
            "HTTP Digest challenge does not support qop=auth".into(),
        ));
    }
    let algorithm = parameters
        .get("algorithm")
        .map_or("MD5", String::as_str)
        .to_ascii_uppercase();
    if algorithm != "MD5" {
        return Err(ProbeError::Unsupported(format!(
            "HTTP Digest algorithm {algorithm} is not supported"
        )));
    }
    Ok(DigestChallenge {
        realm,
        nonce,
        opaque: parameters.get("opaque").cloned(),
        algorithm,
    })
}

fn parse_digest_parameters(value: &str) -> std::collections::HashMap<String, String> {
    let mut parameters = std::collections::HashMap::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars().chain(std::iter::once(',')) {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
            current.push(character);
        } else if character == ',' && !quoted {
            if let Some((key, value)) = current.trim().split_once('=') {
                let value = value.trim().trim_matches('"').replace("\\\"", "\"");
                parameters.insert(key.trim().to_ascii_lowercase(), value);
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    parameters
}

fn digest_authorization(
    credentials: &Credentials,
    endpoint: &Url,
    challenge: &DigestChallenge,
    method: &str,
    nonce_count: u32,
) -> Result<String, ProbeError> {
    let mut random = [0_u8; 16];
    rand::rng().fill_bytes(&mut random);
    let cnonce = hex(&random);
    digest_authorization_with_cnonce(
        credentials,
        endpoint,
        challenge,
        method,
        nonce_count,
        &cnonce,
    )
}

fn digest_authorization_with_cnonce(
    credentials: &Credentials,
    endpoint: &Url,
    challenge: &DigestChallenge,
    method: &str,
    nonce_count: u32,
    cnonce: &str,
) -> Result<String, ProbeError> {
    let mut uri = endpoint.path().to_owned();
    if let Some(query) = endpoint.query() {
        uri.push('?');
        uri.push_str(query);
    }
    if uri.is_empty() {
        uri.push('/');
    }
    let ha1 = md5_hex(&format!(
        "{}:{}:{}",
        credentials.username, challenge.realm, credentials.password
    ));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let nc = format!("{nonce_count:08x}");
    let response = md5_hex(&format!(
        "{ha1}:{}:{nc}:{cnonce}:auth:{ha2}",
        challenge.nonce
    ));
    let mut header = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{response}\", algorithm={}, qop=auth, nc={nc}, cnonce=\"{cnonce}\"",
        escape_digest(&credentials.username),
        escape_digest(&challenge.realm),
        escape_digest(&challenge.nonce),
        escape_digest(&uri),
        challenge.algorithm,
    );
    if let Some(opaque) = &challenge.opaque {
        write!(header, ", opaque=\"{}\"", escape_digest(opaque))
            .map_err(|error| ProbeError::Transport(error.to_string()))?;
    }
    Ok(header)
}

fn md5_hex(value: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(value.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn escape_digest(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn soap_fault_error(xml: &str) -> Option<ProbeError> {
    let document = Document::parse(xml).ok()?;
    let fault = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "Fault")?;
    let values = fault
        .descendants()
        .filter_map(|node| node.text())
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = values.to_ascii_lowercase();
    if normalized.contains("notauthorized") {
        Some(ProbeError::Authentication)
    } else if normalized.contains("transmodeerror") {
        Some(ProbeError::TransportMode)
    } else {
        Some(ProbeError::Unsupported(
            find_text(fault, "Text")
                .unwrap_or("unspecified SOAP fault")
                .to_owned(),
        ))
    }
}

pub struct OnvifClient<T> {
    transport: T,
    device_endpoint: Url,
    credentials: Option<Credentials>,
    allowed_service_host: Option<String>,
}

impl<T: SoapTransport> OnvifClient<T> {
    pub fn new(
        transport: T,
        device_endpoint: Url,
        credentials: Option<Credentials>,
        allowed_service_host: Option<String>,
    ) -> Self {
        Self {
            transport,
            device_endpoint,
            credentials,
            allowed_service_host,
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
            Err(ProbeError::Authentication | ProbeError::HttpUnauthorized) => {
                capabilities.push(capability(
                    CapabilityKind::DeviceInformation,
                    SupportStatus::AuthenticationRequired,
                    "device rejected unauthenticated or supplied credentials",
                ));
                None
            }
            Err(ProbeError::InvalidCredentials) => return Err(ProbeError::InvalidCredentials),
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
            Err(ProbeError::Authentication | ProbeError::HttpUnauthorized) => {
                append_auth_required_capabilities(&mut capabilities);
                return Ok(CompatibilityReport {
                    device,
                    services: Vec::new(),
                    media_profiles: Vec::new(),
                    capabilities,
                });
            }
            Err(ProbeError::InvalidCredentials) => return Err(ProbeError::InvalidCredentials),
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
                    SupportStatus::Unverified
                } else {
                    SupportStatus::Unsupported
                },
                if supported {
                    format!(
                        "GetServices advertised {needle}; the service operation was not executed"
                    )
                } else {
                    format!("GetServices did not advertise {needle}")
                },
            ));
        }

        let media_service = services.iter().find(|service| {
            service.namespace.contains("/media/wsdl") || service.namespace.contains("/media2/wsdl")
        });
        let media_profiles = if let Some(service) = media_service {
            match validated_service_endpoint(
                &self.device_endpoint,
                &service.endpoint,
                self.allowed_service_host.as_deref(),
            ) {
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
                    Err(ProbeError::Authentication | ProbeError::HttpUnauthorized) => {
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
                    Err(ProbeError::InvalidCredentials) => {
                        return Err(ProbeError::InvalidCredentials);
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
            .call_service(
                &self.device_endpoint,
                &format!("{DEVICE_NS}/GetDeviceInformation"),
                &body,
            )
            .await?;
        parse_device_information(&response)
    }

    pub(crate) async fn services(&self) -> Result<Vec<ServiceEndpoint>, ProbeError> {
        let body = format!(
            "<tds:GetServices xmlns:tds=\"{DEVICE_NS}\"><tds:IncludeCapability>true</tds:IncludeCapability></tds:GetServices>"
        );
        let response = self
            .call_service(
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
            .call_service(endpoint, &format!("{namespace}/GetProfiles"), &body)
            .await?;
        parse_media_profiles(&response)
    }

    pub(crate) async fn call_service(
        &self,
        endpoint: &Url,
        action: &str,
        body: &str,
    ) -> Result<String, ProbeError> {
        self.transport
            .post(
                endpoint,
                action,
                soap_envelope(body, self.credentials.as_ref()),
            )
            .await
    }

    pub(crate) fn validated_service_endpoint(&self, value: &str) -> Result<Url, ProbeError> {
        validated_service_endpoint(
            &self.device_endpoint,
            value,
            self.allowed_service_host.as_deref(),
        )
    }

    pub(crate) fn validated_replay_uri(&self, value: &str) -> Result<Url, ProbeError> {
        let uri = Url::parse(value)
            .map_err(|error| ProbeError::Malformed(format!("invalid replay URI: {error}")))?;
        if !matches!(uri.scheme(), "rtsp" | "rtsps") || uri.host_str().is_none() {
            return Err(ProbeError::Malformed(
                "replay URI must be an absolute RTSP or RTSPS URL".into(),
            ));
        }
        if !uri.username().is_empty() || uri.password().is_some() {
            return Err(ProbeError::Malformed(
                "replay URI must not embed credentials".into(),
            ));
        }
        let device_host = self.device_endpoint.host_str().unwrap_or_default();
        let replay_host = uri.host_str().unwrap_or_default();
        let explicitly_allowed = self
            .allowed_service_host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case(replay_host));
        if !device_host.eq_ignore_ascii_case(replay_host) && !explicitly_allowed {
            return Err(ProbeError::Transport(
                "refusing a replay URI hosted outside the selected device; use --allow-service-host to approve one host"
                    .into(),
            ));
        }
        Ok(uri)
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

fn validated_service_endpoint(
    device_endpoint: &Url,
    value: &str,
    allowed_service_host: Option<&str>,
) -> Result<Url, ProbeError> {
    let endpoint = Url::parse(value)
        .map_err(|error| ProbeError::Malformed(format!("invalid service XAddr: {error}")))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err(ProbeError::Malformed(
            "service XAddr must be an absolute HTTP(S) URL".into(),
        ));
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(ProbeError::Malformed(
            "service XAddr must not contain credentials".into(),
        ));
    }
    if device_endpoint.scheme() == "https" && endpoint.scheme() != "https" {
        return Err(ProbeError::Transport(
            "refusing an HTTPS-to-HTTP ONVIF service downgrade".into(),
        ));
    }
    let device_host = device_endpoint.host_str().unwrap_or_default();
    let service_host = endpoint.host_str().unwrap_or_default();
    let explicitly_allowed =
        allowed_service_host.is_some_and(|host| host.eq_ignore_ascii_case(service_host));
    if !device_host.eq_ignore_ascii_case(service_host) && !explicitly_allowed {
        return Err(ProbeError::Transport(
            "refusing a service XAddr hosted outside the selected device; use --allow-service-host to approve one host"
                .into(),
        ));
    }
    Ok(endpoint)
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
    use std::{collections::VecDeque, sync::Mutex, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

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
            SupportStatus::Unverified
        );
        assert_eq!(status(CapabilityKind::Replay), SupportStatus::Unverified);
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

    #[test]
    fn parses_and_calculates_digest_qop_auth() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::WWW_AUTHENTICATE,
            "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""
                .parse()
                .unwrap(),
        );
        let challenge = digest_challenge(&headers).unwrap();
        let header = digest_authorization_with_cnonce(
            &Credentials {
                username: "Mufasa".into(),
                password: "Circle Of Life".into(),
            },
            &Url::parse("http://www.example.com/dir/index.html").unwrap(),
            &challenge,
            "GET",
            1,
            "0a4f113b",
        )
        .unwrap();
        assert!(header.contains("qop=auth"));
        assert!(header.contains("response=\"6629fae49393a05397450978507c4ef1\""));
        assert!(!header.contains("Circle Of Life"));
    }

    #[tokio::test]
    async fn http_transport_retries_with_digest_authorization() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut first_request = [0_u8; 4096];
            let bytes = first.read(&mut first_request).await.unwrap();
            assert!(
                std::str::from_utf8(&first_request[..bytes])
                    .unwrap()
                    .contains("POST /onvif")
            );
            first
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"camera\", nonce=\"nonce\", qop=\"auth\"\r\nContent-Length: 9\r\nConnection: close\r\n\r\nchallenge")
                .await
                .unwrap();

            let (mut second, _) = listener.accept().await.unwrap();
            let mut second_request = [0_u8; 4096];
            let bytes = second.read(&mut second_request).await.unwrap();
            let request = std::str::from_utf8(&second_request[..bytes]).unwrap();
            let normalized = request.to_ascii_lowercase();
            assert!(normalized.contains("authorization: digest"));
            assert!(normalized.contains("qop=auth"));
            assert!(!request.contains("s3cret"));
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n<ok/>",
                )
                .await
                .unwrap();
        });

        let transport = HttpSoapTransport::new(
            Duration::from_secs(2),
            Some(Credentials {
                username: "operator".into(),
                password: "s3cret".into(),
            }),
            false,
            None,
        )
        .unwrap();
        let endpoint = Url::parse(&format!("http://{address}/onvif/device_service")).unwrap();
        assert_eq!(
            transport
                .post(&endpoint, "urn:test", "<body/>".into())
                .await
                .unwrap(),
            "<ok/>"
        );
        server.await.unwrap();
    }

    #[test]
    fn classifies_soap_transport_mode_and_tls_failures() {
        let trans_mode = r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"><s:Body><s:Fault><s:Code><s:Value>ter:TransModeError</s:Value></s:Code></s:Fault></s:Body></s:Envelope>"#;
        assert!(matches!(
            soap_fault_error(trans_mode),
            Some(ProbeError::TransportMode)
        ));
        assert!(matches!(
            classify_transport_message("error trying to connect: invalid peer certificate"),
            ProbeError::Tls(_)
        ));
    }

    #[test]
    fn rejects_service_https_downgrade() {
        let device = Url::parse("https://camera.test/onvif/device_service").unwrap();
        assert!(
            validated_service_endpoint(&device, "http://camera.test/onvif/media", None).is_err()
        );
    }

    #[test]
    fn rejects_missing_explicit_ca_file() {
        let result = HttpSoapTransport::new(
            Duration::from_secs(1),
            None,
            false,
            Some(std::path::Path::new("/tmp/openpanora-no-such-ca.pem")),
        );
        assert!(matches!(result, Err(ProbeError::Input(_))));
    }
}
