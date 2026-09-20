use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AreaId, CaptureBackend, SyncMode};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    Starting,
    #[default]
    Ready,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeState {
    #[default]
    Unconfigured,
    Disconnected,
    Discovering,
    Pairing,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureState {
    #[default]
    Idle,
    RequestingPermission,
    Capturing,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncState {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    InvalidRequest,
    InvalidConfiguration,
    VersionMismatch,
    BridgeNotFound,
    BridgeUnavailable,
    LinkButtonRequired,
    AreaNotFound,
    DisplayNotFound,
    CapturePermissionDenied,
    CaptureUnavailable,
    SyncUnavailable,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionableError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl ActionableError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            action: None,
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub service: ServiceState,
    pub bridge: BridgeState,
    pub capture: CaptureState,
    pub sync: SyncState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_area: Option<AreaId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames_per_second: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ActionableError>,
}

impl Default for StatusSnapshot {
    fn default() -> Self {
        Self {
            service: ServiceState::Ready,
            bridge: BridgeState::Unconfigured,
            capture: CaptureState::Idle,
            sync: SyncState::Stopped,
            active_area: None,
            frames_per_second: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub protocol_versions: Vec<u32>,
    pub capture_backends: Vec<CaptureBackend>,
    pub sync_modes: Vec<SyncMode>,
    pub audio_reactive: bool,
    pub discovery: bool,
    /// The portal chooser selects a monitor; it cannot target `selected_display` by ID.
    pub portal_display_selection: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            protocol_versions: vec![crate::PROTOCOL_VERSION],
            capture_backends: vec![CaptureBackend::PortalPipewire, CaptureBackend::Grim],
            sync_modes: vec![SyncMode::Video, SyncMode::Game],
            audio_reactive: false,
            discovery: true,
            portal_display_selection: false,
        }
    }
}
