use lightsync_domain::{
    BridgeState, CaptureState, ErrorCode, ServiceState, StatusSnapshot, SyncState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashboardState {
    pub service_id: &'static str,
    pub bridge_id: &'static str,
    pub capture_id: &'static str,
    pub sync_id: &'static str,
    pub action_id: &'static str,
    pub action_enabled: bool,
    pub action_is_stop: bool,
    pub error_id: Option<&'static str>,
}

pub fn dashboard_state(connected: bool, status: Option<&StatusSnapshot>) -> DashboardState {
    let Some(status) = status.filter(|_| connected) else {
        return DashboardState {
            service_id: "state-reconnecting",
            bridge_id: "state-unknown",
            capture_id: "state-unknown",
            sync_id: "state-unknown",
            action_id: "action-start",
            action_enabled: false,
            action_is_stop: false,
            error_id: Some("error-daemon-unavailable"),
        };
    };
    let action_is_stop = matches!(status.sync, SyncState::Starting | SyncState::Running);
    DashboardState {
        service_id: service_id(status.service),
        bridge_id: bridge_id(status.bridge),
        capture_id: capture_id(status.capture),
        sync_id: sync_id(status.sync),
        action_id: if action_is_stop {
            "action-stop"
        } else {
            "action-start"
        },
        action_enabled: !matches!(status.sync, SyncState::Starting | SyncState::Stopping),
        action_is_stop,
        error_id: status.error.as_ref().map(|error| error_id(error.code)),
    }
}

pub fn service_id(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Starting => "state-starting",
        ServiceState::Ready => "state-ready",
        ServiceState::Stopping => "state-stopping",
        ServiceState::Failed => "state-failed",
    }
}

pub fn bridge_id(state: BridgeState) -> &'static str {
    match state {
        BridgeState::Unconfigured => "state-unconfigured",
        BridgeState::Disconnected => "state-disconnected",
        BridgeState::Discovering => "state-discovering",
        BridgeState::Pairing => "state-pairing",
        BridgeState::Connecting => "state-connecting",
        BridgeState::Connected => "state-connected",
    }
}

pub fn capture_id(state: CaptureState) -> &'static str {
    match state {
        CaptureState::Idle => "state-idle",
        CaptureState::RequestingPermission => "state-requesting-permission",
        CaptureState::Capturing => "state-capturing",
        CaptureState::Failed => "state-failed",
    }
}

pub fn sync_id(state: SyncState) -> &'static str {
    match state {
        SyncState::Stopped => "state-stopped",
        SyncState::Starting => "state-starting",
        SyncState::Running => "state-running",
        SyncState::Stopping => "state-stopping",
        SyncState::Failed => "state-failed",
    }
}

pub fn error_id(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::BridgeNotFound | ErrorCode::BridgeUnavailable => "error-bridge-unavailable",
        ErrorCode::LinkButtonRequired => "error-link-button",
        ErrorCode::AreaNotFound => "error-area",
        ErrorCode::DisplayNotFound => "error-display",
        ErrorCode::CapturePermissionDenied => "error-capture-permission",
        ErrorCode::CaptureUnavailable => "error-capture-unavailable",
        ErrorCode::SyncUnavailable => "error-sync-unavailable",
        ErrorCode::InvalidRequest
        | ErrorCode::InvalidConfiguration
        | ErrorCode::VersionMismatch
        | ErrorCode::Conflict
        | ErrorCode::Internal => "error-generic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_state_is_reconnecting_and_safe() {
        let state = dashboard_state(false, None);
        assert_eq!(state.service_id, "state-reconnecting");
        assert!(!state.action_enabled);
        assert!(!state.action_is_stop);
    }

    #[test]
    fn running_state_maps_to_stop_action() {
        let status = StatusSnapshot {
            sync: SyncState::Running,
            bridge: BridgeState::Connected,
            ..StatusSnapshot::default()
        };
        let state = dashboard_state(true, Some(&status));
        assert_eq!(state.action_id, "action-stop");
        assert!(state.action_enabled);
        assert!(state.action_is_stop);
    }

    #[test]
    fn transitional_state_disables_action() {
        let status = StatusSnapshot {
            sync: SyncState::Stopping,
            ..StatusSnapshot::default()
        };
        assert!(!dashboard_state(true, Some(&status)).action_enabled);
    }
}
