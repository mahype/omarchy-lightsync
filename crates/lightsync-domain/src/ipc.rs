use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    ActionableError, AppConfig, AreaId, BridgeId, Capabilities, ConfigUpdate, EntertainmentArea,
    Profile, ProfileId, StatusSnapshot,
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredBridge {
    pub id: BridgeId,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub version: u32,
    pub id: Uuid,
    #[serde(flatten)]
    pub request: Request,
}

impl RequestEnvelope {
    pub fn new(request: Request) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: Uuid::new_v4(),
            request,
        }
    }

    pub fn validate_version(&self) -> Result<(), ProtocolError> {
        validate_version(self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    GetStatus,
    GetConfig,
    GetCapabilities,
    DiscoverBridges,
    BeginPairing { bridge: DiscoveredBridge },
    CompletePairing { bridge_id: BridgeId },
    ForgetBridge,
    RefreshAreas,
    SelectArea { area_id: Option<AreaId> },
    UpdateConfig { update: ConfigUpdate },
    CreateProfile { profile: Profile },
    UpdateProfile { profile: Profile },
    DeleteProfile { profile_id: ProfileId },
    ActivateProfile { profile_id: Option<ProfileId> },
    Start,
    Stop,
    Toggle,
    WatchStatus { enabled: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub version: u32,
    pub id: Uuid,
    #[serde(flatten)]
    pub response: Response,
}

impl ResponseEnvelope {
    pub fn success(id: Uuid, payload: ResponsePayload) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            response: Response::Success { payload },
        }
    }

    pub fn error(id: Uuid, error: ActionableError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            response: Response::Error { error },
        }
    }

    pub fn validate_version(&self) -> Result<(), ProtocolError> {
        validate_version(self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Success { payload: ResponsePayload },
    Error { error: ActionableError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ResponsePayload {
    Acknowledged,
    Status(StatusSnapshot),
    Config(AppConfig),
    Capabilities(Capabilities),
    Bridges(Vec<DiscoveredBridge>),
    Areas(Vec<EntertainmentArea>),
    Profile(Profile),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub version: u32,
    #[serde(flatten)]
    pub event: Event,
}

impl EventEnvelope {
    pub fn new(event: Event) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            event,
        }
    }

    pub fn validate_version(&self) -> Result<(), ProtocolError> {
        validate_version(self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    StatusChanged(StatusSnapshot),
    ConfigChanged {
        revision: u64,
    },
    AreasChanged(Vec<EntertainmentArea>),
    PairingProgress {
        bridge_id: BridgeId,
        waiting_for_link_button: bool,
    },
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("JSON codec error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("an NDJSON record must contain exactly one JSON value")]
    MultipleRecords,
    #[error("unsupported protocol version {received}; expected {expected}")]
    VersionMismatch { expected: u32, received: u32 },
}

pub fn to_ndjson<T: Serialize>(value: &T) -> Result<String, ProtocolError> {
    let mut json = serde_json::to_string(value)?;
    json.push('\n');
    Ok(json)
}

pub fn from_ndjson<T: DeserializeOwned>(record: &str) -> Result<T, ProtocolError> {
    let record = record.strip_suffix('\n').unwrap_or(record);
    let record = record.strip_suffix('\r').unwrap_or(record);
    if record.contains(['\n', '\r']) {
        return Err(ProtocolError::MultipleRecords);
    }
    Ok(serde_json::from_str(record)?)
}

fn validate_version(version: u32) -> Result<(), ProtocolError> {
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            received: version,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CaptureState, ServiceState, SyncState};

    #[test]
    fn request_round_trips_as_one_ndjson_record() {
        let request = RequestEnvelope::new(Request::WatchStatus { enabled: true });
        let encoded = to_ndjson(&request).expect("encode request");
        assert!(encoded.ends_with('\n'));
        assert_eq!(encoded.lines().count(), 1);
        assert_eq!(
            from_ndjson::<RequestEnvelope>(&encoded).expect("decode request"),
            request
        );
    }

    #[test]
    fn protocol_tolerates_unknown_envelope_fields() {
        let id = Uuid::nil();
        let json = format!(r#"{{"version":1,"id":"{id}","method":"get_status","future":true}}"#);
        let request: RequestEnvelope = from_ndjson(&json).expect("decode future request");
        assert_eq!(request.request, Request::GetStatus);
    }

    #[test]
    fn response_and_event_are_typed() {
        let status = StatusSnapshot {
            service: ServiceState::Ready,
            capture: CaptureState::Capturing,
            sync: SyncState::Running,
            ..StatusSnapshot::default()
        };
        let response =
            ResponseEnvelope::success(Uuid::nil(), ResponsePayload::Status(status.clone()));
        let event = EventEnvelope::new(Event::StatusChanged(status));
        assert!(
            to_ndjson(&response)
                .expect("response JSON")
                .contains("\"result\":\"success\"")
        );
        assert!(
            to_ndjson(&event)
                .expect("event JSON")
                .contains("\"event\":\"status_changed\"")
        );
    }

    #[test]
    fn version_and_record_count_are_validated() {
        let mut request = RequestEnvelope::new(Request::GetStatus);
        request.version = 2;
        assert!(matches!(
            request.validate_version(),
            Err(ProtocolError::VersionMismatch { .. })
        ));
        assert!(matches!(
            from_ndjson::<RequestEnvelope>("{}\n{}\n"),
            Err(ProtocolError::MultipleRecords)
        ));
    }
}
