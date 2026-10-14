use crate::onvif::{OnvifClient, ProbeError, SoapTransport};
use chrono::{DateTime, Utc};
use quick_xml::escape::escape;
use roxmltree::{Document, Node};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use url::Url;

const SEARCH_NS: &str = "http://www.onvif.org/ver10/search/wsdl";
const RECORDING_NS: &str = "http://www.onvif.org/ver10/recording/wsdl";
const REPLAY_NS: &str = "http://www.onvif.org/ver10/replay/wsdl";
const SCHEMA_NS: &str = "http://www.onvif.org/ver10/schema";
const MAX_SEARCH_PAGES: usize = 8;
const PAGE_SIZE: u32 = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingSummary {
    pub data_from: Option<DateTime<Utc>>,
    pub data_until: Option<DateTime<Utc>>,
    pub number_of_recordings: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingInfo {
    pub token: String,
    pub earliest_recording: Option<DateTime<Utc>>,
    pub latest_recording: Option<DateTime<Utc>>,
    pub track_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingSearchReport {
    pub recordings: Vec<RecordingInfo>,
    pub complete: bool,
    pub interval_filter_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayUri {
    uri: Url,
}

impl ReplayUri {
    pub fn url(&self) -> &Url {
        &self.uri
    }

    pub fn display(&self) -> String {
        let mut safe = self.uri.clone();
        safe.set_query(None);
        safe.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayConfiguration {
    pub session_timeout: Option<String>,
}

pub struct RecordingClient<'a, T> {
    client: &'a OnvifClient<T>,
    deadline: Instant,
}

impl<'a, T: SoapTransport> RecordingClient<'a, T> {
    pub fn new(client: &'a OnvifClient<T>, timeout: Duration) -> Self {
        Self {
            client,
            deadline: Instant::now() + timeout,
        }
    }

    pub async fn summary(&self) -> Result<RecordingSummary, ProbeError> {
        let endpoint = self.search_endpoint().await?;
        let response = self
            .call(
                &endpoint,
                SEARCH_NS,
                "GetRecordingSummary",
                "<tse:GetRecordingSummary/>",
            )
            .await?;
        parse_summary(&response)
    }

    pub async fn list(&self) -> Result<Vec<RecordingInfo>, ProbeError> {
        let endpoint = self.recording_endpoint().await?;
        let response = self
            .call(
                &endpoint,
                RECORDING_NS,
                "GetRecordings",
                "<trc:GetRecordings/>",
            )
            .await?;
        parse_recording_items(&response)
    }

    pub async fn search(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: u32,
    ) -> Result<RecordingSearchReport, ProbeError> {
        let cancellation = AtomicBool::new(false);
        self.search_with_cancellation(start, end, limit, &cancellation)
            .await
    }

    pub async fn search_with_cancellation(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: u32,
        cancellation: &AtomicBool,
    ) -> Result<RecordingSearchReport, ProbeError> {
        self.ensure_before_deadline(cancellation)?;
        if let (Some(start), Some(end)) = (start, end)
            && end <= start
        {
            return Err(ProbeError::Input(
                "--end must be later than --start for interval filtering".into(),
            ));
        }
        let endpoint = self.search_endpoint().await?;
        let find_body = format!(
            "<tse:FindRecordings xmlns:tse=\"{SEARCH_NS}\"><tse:Scope/>\
             <tse:MaxMatches>{limit}</tse:MaxMatches><tse:KeepAliveTime>PT10S</tse:KeepAliveTime>\
             </tse:FindRecordings>"
        );
        let response = self
            .call(&endpoint, SEARCH_NS, "FindRecordings", &find_body)
            .await?;
        let token = parse_search_token(&response)?;

        let result = self
            .collect_search_results(&endpoint, &token, cancellation)
            .await;
        // EndSearch is always attempted once FindRecordings created a session. A completed
        // search may correctly reject it as an already-invalid token, so cleanup cannot hide
        // a successful result.
        let cleanup = self.end_search(&endpoint, &token).await;
        let mut recordings = result?;
        if let Err(error) = cleanup
            && !matches!(error, ProbeError::Unsupported(_))
        {
            // The query result remains valid. Cleanup faults are deliberately not treated as
            // query success, because callers need to know that the session may have expired.
            return Err(error);
        }

        let mut unique = BTreeMap::new();
        for recording in recordings.drain(..) {
            unique.insert(recording.token.clone(), recording);
        }
        let mut recordings: Vec<_> = unique.into_values().collect();
        for recording in &mut recordings {
            if recording.earliest_recording.is_none() && recording.latest_recording.is_none() {
                if let Ok(details) = self
                    .recording_information(&endpoint, &recording.token)
                    .await
                {
                    *recording = details;
                }
            }
        }
        if start.is_some() || end.is_some() {
            recordings.retain(|recording| overlaps(recording, start, end));
        }
        Ok(RecordingSearchReport {
            recordings,
            complete: true,
            interval_filter_applied: start.is_some() || end.is_some(),
        })
    }

    pub async fn replay_uri(&self, recording_token: &str) -> Result<ReplayUri, ProbeError> {
        require_token(recording_token, "recording token")?;
        let endpoint = self.replay_endpoint().await?;
        let body = format!(
            "<trp:GetReplayUri xmlns:trp=\"{REPLAY_NS}\" xmlns:tt=\"{SCHEMA_NS}\">\
             <trp:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport>\
             <tt:Protocol>RTSP</tt:Protocol></tt:Transport></trp:StreamSetup>\
             <trp:RecordingToken>{}</trp:RecordingToken></trp:GetReplayUri>",
            escape(recording_token)
        );
        let response = self
            .call(&endpoint, REPLAY_NS, "GetReplayUri", &body)
            .await?;
        let value = parse_replay_uri(&response)?;
        Ok(ReplayUri {
            uri: self.client.validated_replay_uri(&value)?,
        })
    }

    pub async fn replay_configuration(&self) -> Result<ReplayConfiguration, ProbeError> {
        let endpoint = self.replay_endpoint().await?;
        let response = self
            .call(
                &endpoint,
                REPLAY_NS,
                "GetReplayConfiguration",
                "<trp:GetReplayConfiguration/>",
            )
            .await?;
        parse_replay_configuration(&response)
    }

    async fn collect_search_results(
        &self,
        endpoint: &Url,
        token: &str,
        cancellation: &AtomicBool,
    ) -> Result<Vec<RecordingInfo>, ProbeError> {
        let mut recordings = Vec::new();
        for _ in 0..MAX_SEARCH_PAGES {
            self.ensure_before_deadline(cancellation)?;
            let body = format!(
                "<tse:GetRecordingSearchResults xmlns:tse=\"{SEARCH_NS}\">\
                 <tse:SearchToken>{}</tse:SearchToken><tse:MinResults>0</tse:MinResults>\
                 <tse:MaxResults>{PAGE_SIZE}</tse:MaxResults><tse:WaitTime>PT1S</tse:WaitTime>\
                 </tse:GetRecordingSearchResults>",
                escape(token)
            );
            let response = self
                .call(endpoint, SEARCH_NS, "GetRecordingSearchResults", &body)
                .await?;
            let page = parse_search_results(&response)?;
            recordings.extend(page.recordings);
            if page.completed {
                return Ok(recordings);
            }
        }
        Err(ProbeError::Timeout)
    }

    async fn recording_information(
        &self,
        endpoint: &Url,
        token: &str,
    ) -> Result<RecordingInfo, ProbeError> {
        let body = format!(
            "<tse:GetRecordingInformation xmlns:tse=\"{SEARCH_NS}\">\
             <tse:RecordingToken>{}</tse:RecordingToken></tse:GetRecordingInformation>",
            escape(token)
        );
        let response = self
            .call(endpoint, SEARCH_NS, "GetRecordingInformation", &body)
            .await?;
        parse_recording_information(&response)
    }

    async fn end_search(&self, endpoint: &Url, token: &str) -> Result<(), ProbeError> {
        let body = format!(
            "<tse:EndSearch xmlns:tse=\"{SEARCH_NS}\"><tse:SearchToken>{}</tse:SearchToken>\
             </tse:EndSearch>",
            escape(token)
        );
        self.call(endpoint, SEARCH_NS, "EndSearch", &body).await?;
        Ok(())
    }

    async fn search_endpoint(&self) -> Result<Url, ProbeError> {
        self.service_endpoint("/search/wsdl").await
    }

    async fn recording_endpoint(&self) -> Result<Url, ProbeError> {
        self.service_endpoint("/recording/wsdl").await
    }

    async fn replay_endpoint(&self) -> Result<Url, ProbeError> {
        self.service_endpoint("/replay/wsdl").await
    }

    async fn service_endpoint(&self, needle: &str) -> Result<Url, ProbeError> {
        let services = self.client.services().await?;
        let service = services
            .iter()
            .find(|service| service.namespace.contains(needle))
            .ok_or_else(|| {
                ProbeError::Unsupported(format!("GetServices did not advertise {needle}"))
            })?;
        self.client.validated_service_endpoint(&service.endpoint)
    }

    async fn call(
        &self,
        endpoint: &Url,
        namespace: &str,
        operation: &str,
        body: &str,
    ) -> Result<String, ProbeError> {
        self.ensure_before_deadline(&AtomicBool::new(false))?;
        self.client
            .call_service(endpoint, &format!("{namespace}/{operation}"), body)
            .await
    }

    fn ensure_before_deadline(&self, cancellation: &AtomicBool) -> Result<(), ProbeError> {
        if cancellation.load(Ordering::Acquire) {
            return Err(ProbeError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            Err(ProbeError::Timeout)
        } else {
            Ok(())
        }
    }
}

struct SearchResultsPage {
    recordings: Vec<RecordingInfo>,
    completed: bool,
}

fn parse_summary(xml: &str) -> Result<RecordingSummary, ProbeError> {
    let document = parse_document(xml)?;
    let summary = find_element(document.root(), "Summary")
        .ok_or_else(|| ProbeError::Malformed("GetRecordingSummary omitted Summary".into()))?;
    Ok(RecordingSummary {
        data_from: optional_time(summary, "DataFrom")?,
        data_until: optional_time(summary, "DataUntil")?,
        number_of_recordings: child_text(summary, "NumberRecordings")
            .ok_or_else(|| {
                ProbeError::Malformed("RecordingSummary omitted NumberRecordings".into())
            })?
            .parse()
            .map_err(|error| ProbeError::Malformed(format!("invalid NumberRecordings: {error}")))?,
    })
}

fn parse_recording_items(xml: &str) -> Result<Vec<RecordingInfo>, ProbeError> {
    let document = parse_document(xml)?;
    let recordings: Vec<_> = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "RecordingItem")
        .map(recording_info_from_node)
        .collect::<Result<_, _>>()?;
    if recordings.is_empty() && !has_response(document.root(), "GetRecordingsResponse") {
        return Err(ProbeError::Malformed(
            "expected GetRecordingsResponse".into(),
        ));
    }
    Ok(recordings)
}

fn parse_search_token(xml: &str) -> Result<String, ProbeError> {
    let document = parse_document(xml)?;
    let response = find_element(document.root(), "FindRecordingsResponse")
        .ok_or_else(|| ProbeError::Malformed("expected FindRecordingsResponse".into()))?;
    let token = child_text(response, "SearchToken")
        .ok_or_else(|| ProbeError::Malformed("FindRecordings omitted SearchToken".into()))?;
    require_token(token, "search token")?;
    Ok(token.to_owned())
}

fn parse_search_results(xml: &str) -> Result<SearchResultsPage, ProbeError> {
    let document = parse_document(xml)?;
    let result_list = find_element(document.root(), "ResultList").ok_or_else(|| {
        ProbeError::Malformed("GetRecordingSearchResults omitted ResultList".into())
    })?;
    let state = child_text(result_list, "SearchState")
        .ok_or_else(|| ProbeError::Malformed("ResultList omitted SearchState".into()))?;
    let recordings = result_list
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "RecordingInformation")
        .map(recording_info_from_node)
        .collect::<Result<_, _>>()?;
    Ok(SearchResultsPage {
        recordings,
        completed: state.eq_ignore_ascii_case("Completed"),
    })
}

fn parse_recording_information(xml: &str) -> Result<RecordingInfo, ProbeError> {
    let document = parse_document(xml)?;
    let info = find_element(document.root(), "RecordingInformation").ok_or_else(|| {
        ProbeError::Malformed("GetRecordingInformation omitted RecordingInformation".into())
    })?;
    recording_info_from_node(info)
}

fn parse_replay_uri(xml: &str) -> Result<String, ProbeError> {
    let document = parse_document(xml)?;
    let response = find_element(document.root(), "GetReplayUriResponse")
        .ok_or_else(|| ProbeError::Malformed("expected GetReplayUriResponse".into()))?;
    let uri = child_text(response, "Uri")
        .ok_or_else(|| ProbeError::Malformed("GetReplayUri omitted Uri".into()))?;
    Ok(uri.to_owned())
}

fn parse_replay_configuration(xml: &str) -> Result<ReplayConfiguration, ProbeError> {
    let document = parse_document(xml)?;
    let response = find_element(document.root(), "GetReplayConfigurationResponse")
        .ok_or_else(|| ProbeError::Malformed("expected GetReplayConfigurationResponse".into()))?;
    Ok(ReplayConfiguration {
        session_timeout: find_element(response, "ReplayConfiguration")
            .and_then(|configuration| child_text(configuration, "SessionTimeout"))
            .map(str::to_owned),
    })
}

fn recording_info_from_node(node: Node<'_, '_>) -> Result<RecordingInfo, ProbeError> {
    let token = child_text(node, "RecordingToken").ok_or_else(|| {
        ProbeError::Malformed("RecordingInformation omitted RecordingToken".into())
    })?;
    require_token(token, "recording token")?;
    Ok(RecordingInfo {
        token: token.to_owned(),
        earliest_recording: optional_time(node, "EarliestRecording")?,
        latest_recording: optional_time(node, "LatestRecording")?,
        track_count: node
            .descendants()
            .filter(|child| child.is_element() && child.tag_name().name() == "TrackInformation")
            .count(),
    })
}

fn overlaps(
    recording: &RecordingInfo,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> bool {
    match (recording.earliest_recording, recording.latest_recording) {
        (Some(first), Some(last)) => {
            end.is_none_or(|end| first < end) && start.is_none_or(|start| last >= start)
        }
        // Unknown bounds must not be represented as a positive interval match.
        _ => false,
    }
}

fn optional_time(node: Node<'_, '_>, name: &str) -> Result<Option<DateTime<Utc>>, ProbeError> {
    child_text(node, name).map(parse_utc).transpose()
}

fn parse_utc(value: &str) -> Result<DateTime<Utc>, ProbeError> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| ProbeError::Malformed(format!("invalid ONVIF UTC timestamp: {error}")))
}

fn parse_document(xml: &str) -> Result<Document<'_>, ProbeError> {
    Document::parse(xml).map_err(|error| ProbeError::Malformed(error.to_string()))
}

fn find_element<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.descendants()
        .find(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
}

fn child_text<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
        .and_then(|candidate| candidate.text())
}

fn has_response(node: Node<'_, '_>, name: &str) -> bool {
    find_element(node, name).is_some()
}

fn require_token(value: &str, label: &str) -> Result<(), ProbeError> {
    if value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(ProbeError::Malformed(format!("invalid {label}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    const SUMMARY: &str = r#"<Envelope><Body><GetRecordingSummaryResponse><Summary><DataFrom>2025-01-01T00:00:00Z</DataFrom><DataUntil>2025-01-02T00:00:00Z</DataUntil><NumberRecordings>2</NumberRecordings></Summary></GetRecordingSummaryResponse></Body></Envelope>"#;
    const SEARCH_RESULTS: &str = r#"<Envelope><Body><GetRecordingSearchResultsResponse><ResultList><SearchState>Completed</SearchState><RecordingInformation><RecordingToken>recording-1</RecordingToken><EarliestRecording>2025-01-01T00:00:00Z</EarliestRecording><LatestRecording>2025-01-01T01:00:00Z</LatestRecording><TrackInformation/><TrackInformation/></RecordingInformation></ResultList></GetRecordingSearchResultsResponse></Body></Envelope>"#;

    #[test]
    fn parses_recording_summary_with_utc_times() {
        let summary = parse_summary(SUMMARY).unwrap();
        assert_eq!(summary.number_of_recordings, 2);
        assert_eq!(
            summary.data_from.unwrap().to_rfc3339(),
            "2025-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn parses_completed_search_results() {
        let page = parse_search_results(SEARCH_RESULTS).unwrap();
        assert!(page.completed);
        assert_eq!(page.recordings[0].token, "recording-1");
        assert_eq!(page.recordings[0].track_count, 2);
    }

    #[test]
    fn accepts_empty_completed_search() {
        let page = parse_search_results(
            "<Envelope><ResultList><SearchState>Completed</SearchState></ResultList></Envelope>",
        )
        .unwrap();
        assert!(page.completed);
        assert!(page.recordings.is_empty());
    }

    #[test]
    fn rejects_malformed_recording_response() {
        assert!(parse_summary("<not xml").is_err());
        assert!(parse_replay_uri("<Envelope/>").is_err());
    }

    #[test]
    fn parses_recording_inventory_and_replay_uri() {
        let inventory = r#"<Envelope><Body><GetRecordingsResponse><RecordingItem><RecordingToken>recording-2</RecordingToken></RecordingItem></GetRecordingsResponse></Body></Envelope>"#;
        assert_eq!(
            parse_recording_items(inventory).unwrap()[0].token,
            "recording-2"
        );
        let replay = r#"<Envelope><Body><GetReplayUriResponse><Uri>rtsp://camera.test/replay?opaque=value</Uri></GetReplayUriResponse></Body></Envelope>"#;
        assert_eq!(
            parse_replay_uri(replay).unwrap(),
            "rtsp://camera.test/replay?opaque=value"
        );
    }

    #[test]
    fn applies_half_open_interval_overlap() {
        let recording = RecordingInfo {
            token: "recording-1".into(),
            earliest_recording: Some(parse_utc("2025-01-01T00:00:00Z").unwrap()),
            latest_recording: Some(parse_utc("2025-01-01T01:00:00Z").unwrap()),
            track_count: 1,
        };
        assert!(overlaps(
            &recording,
            None,
            Some(parse_utc("2025-01-01T00:30:00Z").unwrap())
        ));
        assert!(!overlaps(
            &recording,
            Some(parse_utc("2025-01-01T01:00:01Z").unwrap()),
            None
        ));
    }

    struct MockTransport {
        responses: Mutex<VecDeque<Result<String, ProbeError>>>,
        actions: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl SoapTransport for MockTransport {
        async fn post(
            &self,
            _endpoint: &Url,
            action: &str,
            _envelope: String,
        ) -> Result<String, ProbeError> {
            self.actions.lock().unwrap().push(action.to_owned());
            self.responses.lock().unwrap().pop_front().unwrap()
        }
    }

    const SERVICES: &str = r#"<Envelope><Body><GetServicesResponse>
      <Service><Namespace>http://www.onvif.org/ver10/search/wsdl</Namespace><XAddr>https://camera.test/onvif/search</XAddr></Service>
      <Service><Namespace>http://www.onvif.org/ver10/recording/wsdl</Namespace><XAddr>https://camera.test/onvif/recording</XAddr></Service>
      <Service><Namespace>http://www.onvif.org/ver10/replay/wsdl</Namespace><XAddr>https://camera.test/onvif/replay</XAddr></Service>
      </GetServicesResponse></Body></Envelope>"#;
    const FIND: &str = r#"<Envelope><Body><FindRecordingsResponse><SearchToken>job-1</SearchToken></FindRecordingsResponse></Body></Envelope>"#;
    const IN_PROGRESS: &str = r#"<Envelope><Body><GetRecordingSearchResultsResponse><ResultList><SearchState>Searching</SearchState></ResultList></GetRecordingSearchResultsResponse></Body></Envelope>"#;
    const END: &str = r#"<Envelope><Body><EndSearchResponse/></Body></Envelope>"#;

    fn client(responses: Vec<&str>) -> (OnvifClient<MockTransport>, Arc<Mutex<Vec<String>>>) {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let client = OnvifClient::new(
            MockTransport {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|response| Ok(response.to_owned()))
                        .collect(),
                ),
                actions: actions.clone(),
            },
            Url::parse("https://camera.test/onvif/device_service").unwrap(),
            None,
            None,
        );
        (client, actions)
    }

    #[tokio::test]
    async fn search_paginates_and_ends_session() {
        let (client, actions) = client(vec![SERVICES, FIND, IN_PROGRESS, SEARCH_RESULTS, END]);
        let report = RecordingClient::new(&client, Duration::from_secs(2))
            .search(None, None, 4)
            .await
            .unwrap();
        assert!(report.complete);
        assert_eq!(report.recordings.len(), 1);
        let actions = actions.lock().unwrap();
        assert!(actions.iter().any(|action| action.ends_with("/EndSearch")));
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.ends_with("/GetRecordingSearchResults"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn rejects_foreign_replay_uri() {
        let replay = r#"<Envelope><Body><GetReplayUriResponse><Uri>rtsp://other.test/replay</Uri></GetReplayUriResponse></Body></Envelope>"#;
        let (client, _) = client(vec![SERVICES, replay]);
        let error = RecordingClient::new(&client, Duration::from_secs(2))
            .replay_uri("recording-1")
            .await
            .unwrap_err();
        assert!(matches!(error, ProbeError::Transport(_)));
    }

    #[tokio::test]
    async fn cancellation_stops_before_creating_a_search() {
        let (client, actions) = client(Vec::new());
        let cancelled = AtomicBool::new(true);
        let error = RecordingClient::new(&client, Duration::from_secs(2))
            .search_with_cancellation(None, None, 1, &cancelled)
            .await
            .unwrap_err();
        assert!(matches!(error, ProbeError::Cancelled));
        assert!(actions.lock().unwrap().is_empty());
    }

    #[test]
    fn rejects_credential_bearing_replay_uri() {
        let (client, _) = client(Vec::new());
        assert!(
            client
                .validated_replay_uri("rtsp://operator:secret@camera.test/replay")
                .is_err()
        );
    }
}
