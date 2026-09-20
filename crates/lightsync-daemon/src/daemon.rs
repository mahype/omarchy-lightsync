use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use lightsync_capture::{
    Capture, CaptureError, GrimCapture, PixelFormat as CapturePixelFormat, PortalCapture,
    PortalOptions,
};
use lightsync_domain::{
    ActionableError, AppConfig, AreaId, BridgeConfiguration, BridgeId, BridgeState, Capabilities,
    CaptureBackend, CaptureState, ConfigUpdate, DiscoveredBridge, EntertainmentArea,
    EntertainmentChannel, ErrorCode, Event, EventEnvelope, NormalizedPoint, Profile, ProfileId,
    Request, ResponsePayload, ServiceState, StatusSnapshot, SyncMode, SyncSettings, SyncState,
};
use lightsync_engine::{ColorProcessor, FrameView, PixelFormat};
use lightsync_hue::{BridgeCandidate, HueClient, HueStreamSession, PairAttempt};
use tokio::sync::{Mutex, broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::config_store;

const TARGET_FPS: u64 = 30;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const RESOURCE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(35);
const STOP_TIMEOUT: Duration = Duration::from_secs(75);

pub type DispatchResult = Result<ResponsePayload, ActionableError>;

#[async_trait]
trait EntertainmentStream: Send + Sync {
    fn send(&self, colors: Vec<lightsync_hue::ChannelColor>) -> anyhow::Result<()>;
    async fn stop(self: Box<Self>) -> anyhow::Result<()>;
}

#[async_trait]
impl EntertainmentStream for HueStreamSession {
    fn send(&self, colors: Vec<lightsync_hue::ChannelColor>) -> anyhow::Result<()> {
        HueStreamSession::send(self, colors)
    }

    async fn stop(self: Box<Self>) -> anyhow::Result<()> {
        (*self).stop().await
    }
}

struct SyncControl {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct State {
    config: AppConfig,
    status: StatusSnapshot,
    areas: Vec<EntertainmentArea>,
    hue: Option<HueClient>,
    pending_pairing: Option<HueClient>,
    startup_cancel: Option<watch::Sender<bool>>,
    sync: Option<SyncControl>,
}

struct Inner {
    state: Mutex<State>,
    config_path: PathBuf,
    portal_token_path: PathBuf,
    events: broadcast::Sender<EventEnvelope>,
    statuses: watch::Sender<StatusSnapshot>,
    operation_in_progress: AtomicBool,
}

struct OperationGuard {
    inner: Arc<Inner>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.inner
            .operation_in_progress
            .store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Daemon {
    inner: Arc<Inner>,
}

impl Daemon {
    pub async fn load(config_path: PathBuf) -> anyhow::Result<Self> {
        let config = config_store::load(&config_path).await?;
        config_store::save(&config_path, &config).await?;
        let pending_config = config_store::load_pending_pairing(&config_path).await?;
        let pending_pairing = pending_config
            .as_ref()
            .and_then(|bridge| client_from_config(bridge).ok());
        let mut status = StatusSnapshot {
            service: ServiceState::Starting,
            bridge: if config.bridge.is_some() {
                BridgeState::Disconnected
            } else if pending_config.is_some() {
                BridgeState::Pairing
            } else {
                BridgeState::Unconfigured
            },
            ..StatusSnapshot::default()
        };
        let (events, _) = broadcast::channel(64);
        let (statuses, _) = watch::channel(status.clone());
        let portal_token_path = config_path
            .parent()
            .expect("validated config path has a parent")
            .join("portal-restore-token");
        status.service = ServiceState::Ready;
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    config,
                    status,
                    areas: Vec::new(),
                    hue: None,
                    pending_pairing,
                    startup_cancel: None,
                    sync: None,
                }),
                config_path,
                portal_token_path,
                events,
                statuses,
                operation_in_progress: AtomicBool::new(false),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.inner.events.subscribe()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<StatusSnapshot> {
        self.inner.statuses.subscribe()
    }

    pub async fn status(&self) -> StatusSnapshot {
        self.inner.state.lock().await.status.clone()
    }

    pub async fn initialize_bridge(&self) {
        let bridge = self.inner.state.lock().await.config.bridge.clone();
        let Some(bridge) = bridge else { return };
        self.set_bridge_state(BridgeState::Connecting, None).await;
        match client_from_config(&bridge) {
            Ok(client) => match client.verify_authorization().await {
                Ok(()) => {
                    self.inner.state.lock().await.hue = Some(client);
                    self.set_bridge_state(BridgeState::Connected, None).await;
                    if let Err(error) = self.refresh_areas_reserved().await {
                        tracing::warn!(code = ?error.code, "could not prime the Hue area cache");
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "configured Hue bridge verification failed");
                    self.set_bridge_state(
                        BridgeState::Disconnected,
                        Some(classify_hue(
                            "Could not authorize with the configured Hue bridge",
                        )),
                    )
                    .await;
                }
            },
            Err(error) => {
                tracing::warn!(error = %error, "configured Hue bridge is invalid");
                self.set_bridge_state(
                    BridgeState::Disconnected,
                    Some(invalid_configuration(
                        "The configured Hue bridge address is invalid",
                    )),
                )
                .await;
            }
        }
    }

    pub async fn auto_start(&self) {
        let config = self.inner.state.lock().await.config.clone();
        if config.auto_start_sync && config.capture_backend == CaptureBackend::PortalPipewire {
            if let Err(error) = self.start(false).await {
                tracing::info!(code = ?error.code, "automatic synchronization was not started");
            }
        }
    }

    pub async fn dispatch(&self, request: Request) -> DispatchResult {
        match request {
            Request::GetStatus => Ok(ResponsePayload::Status(self.status().await)),
            Request::GetConfig => Ok(ResponsePayload::Config(
                self.inner.state.lock().await.config.clone(),
            )),
            Request::GetCapabilities => Ok(ResponsePayload::Capabilities(Capabilities::default())),
            Request::DiscoverBridges => self.discover_bridges().await,
            Request::BeginPairing { bridge } => self.begin_pairing(bridge).await,
            Request::CompletePairing { bridge_id } => self.complete_pairing(bridge_id).await,
            Request::ForgetBridge => self.forget_bridge().await,
            Request::RefreshAreas => self.refresh_areas().await,
            Request::SelectArea { area_id } => self.select_area(area_id).await,
            Request::UpdateConfig { update } => self.update_config(update).await,
            Request::CreateProfile { profile } => self.create_profile(profile).await,
            Request::UpdateProfile { profile } => self.update_profile(profile).await,
            Request::DeleteProfile { profile_id } => self.delete_profile(profile_id).await,
            Request::ActivateProfile { profile_id } => self.activate_profile(profile_id).await,
            Request::Start => self.start(true).await,
            Request::Stop => self.stop().await,
            Request::Toggle => {
                if matches!(self.status().await.sync, SyncState::Running) {
                    self.stop().await
                } else {
                    self.start(true).await
                }
            }
            Request::WatchStatus { .. } => Ok(ResponsePayload::Acknowledged),
        }
    }

    async fn discover_bridges(&self) -> DispatchResult {
        let _operation = self.reserve_idle("discover bridges").await?;
        self.set_bridge_state(BridgeState::Discovering, None).await;
        let result = HueClient::discover().await;
        let state = self.inner.state.lock().await;
        let bridge_state = if state.hue.is_some() {
            BridgeState::Connected
        } else if state.config.bridge.is_some() {
            BridgeState::Disconnected
        } else {
            BridgeState::Unconfigured
        };
        drop(state);
        self.set_bridge_state(bridge_state, None).await;
        result
            .map(|bridges| {
                ResponsePayload::Bridges(
                    bridges
                        .into_iter()
                        .filter_map(|bridge| {
                            Some(DiscoveredBridge {
                                id: BridgeId::new(bridge.id).ok()?,
                                host: bridge.address.to_string(),
                                name: Some(bridge.name),
                            })
                        })
                        .collect(),
                )
            })
            .map_err(|error| {
                tracing::warn!(error = %error, "Hue discovery failed");
                ActionableError::new(
                    ErrorCode::BridgeNotFound,
                    "No Hue bridge could be discovered",
                )
                .with_action("Enter the bridge address manually or check the network")
                .retryable(true)
            })
    }

    async fn begin_pairing(&self, bridge: DiscoveredBridge) -> DispatchResult {
        let _operation = self.reserve_idle("pair a bridge").await?;
        let candidate = candidate_from_discovered(&bridge)?;
        let client =
            HueClient::new(candidate).map_err(|_| invalid_configuration("Invalid Hue bridge"))?;
        let pending = BridgeConfiguration {
            id: bridge.id.clone(),
            host: bridge.host.clone(),
            name: bridge.name.clone(),
        };
        config_store::save_pending_pairing(&self.inner.config_path, Some(&pending))
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to persist pending pairing candidate");
                ActionableError::new(ErrorCode::Internal, "Could not save pairing progress")
                    .retryable(true)
            })?;
        {
            let mut state = self.inner.state.lock().await;
            state.pending_pairing = Some(client.clone());
            state.status.bridge = BridgeState::Pairing;
            state.status.error = None;
        }
        self.notify_status().await;
        self.pair_attempt(client, bridge.id).await
    }

    async fn complete_pairing(&self, bridge_id: BridgeId) -> DispatchResult {
        let _operation = self.reserve_sync_idle("complete pairing").await?;
        let client = self
            .inner
            .state
            .lock()
            .await
            .pending_pairing
            .clone()
            .ok_or_else(|| conflict("No bridge pairing is in progress"))?;
        if client.candidate().id != bridge_id.as_str() {
            return Err(ActionableError::new(
                ErrorCode::InvalidRequest,
                "The pairing bridge ID does not match",
            ));
        }
        self.pair_attempt(client, bridge_id).await
    }

    async fn pair_attempt(&self, client: HueClient, bridge_id: BridgeId) -> DispatchResult {
        match client.pair().await {
            Ok(PairAttempt::WaitingForLinkButton) => {
                self.emit(Event::PairingProgress {
                    bridge_id,
                    waiting_for_link_button: true,
                });
                Ok(ResponsePayload::Acknowledged)
            }
            Ok(PairAttempt::Paired) => {
                let candidate = client.candidate();
                let bridge = BridgeConfiguration {
                    id: BridgeId::new(candidate.id.clone())
                        .map_err(|_| invalid_configuration("Invalid Hue bridge ID"))?,
                    host: candidate.address.to_string(),
                    name: Some(candidate.name.clone()),
                };
                let mut next = self.inner.state.lock().await.config.clone();
                next.bridge = Some(bridge);
                next.revision = next.revision.saturating_add(1);
                if let Err(error) =
                    config_store::save_pending_pairing(&self.inner.config_path, None).await
                {
                    let _ = client.forget_credentials().await;
                    tracing::error!(error = %error, "failed to clear pending pairing state");
                    return Err(ActionableError::new(
                        ErrorCode::Internal,
                        "Could not finalize pairing progress",
                    ));
                }
                if let Err(error) = self.persist_config(next).await {
                    if let Err(cleanup_error) = client.forget_credentials().await {
                        tracing::warn!(error = %cleanup_error, "failed to remove credentials after configuration save failure");
                    }
                    return Err(error);
                }
                let mut state = self.inner.state.lock().await;
                state.hue = Some(client);
                state.pending_pairing = None;
                state.status.bridge = BridgeState::Connected;
                state.status.error = None;
                let revision = state.config.revision;
                drop(state);
                self.emit(Event::PairingProgress {
                    bridge_id,
                    waiting_for_link_button: false,
                });
                self.emit(Event::ConfigChanged { revision });
                self.notify_status().await;
                Ok(ResponsePayload::Acknowledged)
            }
            Err(error) => {
                tracing::warn!(error = %error, "Hue pairing attempt failed");
                Err(classify_hue("Could not pair with the Hue bridge"))
            }
        }
    }

    async fn forget_bridge(&self) -> DispatchResult {
        let _operation = self.reserve_idle("forget the bridge").await?;
        let (configured, client) = {
            let state = self.inner.state.lock().await;
            let configured = state.config.bridge.clone().or_else(|| {
                state
                    .pending_pairing
                    .as_ref()
                    .map(|client| BridgeConfiguration {
                        id: BridgeId::new(client.candidate().id.clone())
                            .expect("HueClient has a validated bridge ID"),
                        host: client.candidate().address.to_string(),
                        name: Some(client.candidate().name.clone()),
                    })
            });
            let client = state.hue.clone().or_else(|| state.pending_pairing.clone());
            (configured, client)
        };
        let client = match (client, configured.as_ref()) {
            (Some(client), _) => Some(client),
            (None, Some(bridge)) => Some(client_from_config(bridge).map_err(|error| {
                tracing::warn!(error = %error, "configured bridge cannot be opened for credential deletion");
                invalid_configuration("The configured Hue bridge is invalid")
            })?),
            (None, None) => None,
        };
        if let Some(client) = client {
            client.forget_credentials().await.map_err(|error| {
                tracing::warn!(error = %error, "failed to delete Hue credentials");
                ActionableError::new(
                    ErrorCode::BridgeUnavailable,
                    "Could not delete Hue credentials",
                )
                .retryable(true)
            })?;
        }

        let mut next = self.inner.state.lock().await.config.clone();
        next.bridge = None;
        next.selected_area = None;
        next.revision = next.revision.saturating_add(1);
        config_store::save_pending_pairing(&self.inner.config_path, None)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to clear pending pairing state");
                ActionableError::new(ErrorCode::Internal, "Could not clear pairing progress")
            })?;
        let config = self.persist_config(next).await?;
        {
            let mut state = self.inner.state.lock().await;
            state.hue = None;
            state.pending_pairing = None;
            state.areas.clear();
            state.status.bridge = BridgeState::Unconfigured;
            state.status.active_area = None;
            state.status.error = None;
        }
        self.emit(Event::AreasChanged(Vec::new()));
        self.notify_status().await;
        Ok(ResponsePayload::Config(config))
    }

    async fn refresh_areas(&self) -> DispatchResult {
        let _operation = self.reserve_idle("refresh areas").await?;
        self.refresh_areas_reserved().await
    }

    async fn refresh_areas_reserved(&self) -> DispatchResult {
        let client = self.hue_client().await?;
        let summaries = client.areas().await.map_err(|error| {
            tracing::warn!(error = %error, "could not load Hue areas");
            classify_hue("Could not load Hue Entertainment areas")
        })?;
        let mut areas = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let positions = client.area_channels(&summary.id).await.map_err(|error| {
                tracing::warn!(error = %error, area = %summary.id, "could not load Hue channels");
                classify_hue("Could not load channels for a Hue Entertainment area")
            })?;
            let channels = positions
                .into_iter()
                .map(|position| {
                    let x = ((position.x + 1.0) / 2.0).clamp(0.0, 1.0);
                    let y = ((1.0 - position.z) / 2.0).clamp(0.0, 1.0);
                    Ok(EntertainmentChannel {
                        id: u16::from(position.channel_id),
                        name: None,
                        position: NormalizedPoint::new(x, y).map_err(|_| {
                            invalid_configuration("Hue returned an invalid channel position")
                        })?,
                    })
                })
                .collect::<Result<Vec<_>, ActionableError>>()?;
            areas.push(EntertainmentArea {
                id: AreaId::new(summary.id)
                    .map_err(|_| invalid_configuration("Hue returned an invalid area ID"))?,
                name: summary.name,
                channels,
            });
        }
        self.inner.state.lock().await.areas = areas.clone();
        self.emit(Event::AreasChanged(areas.clone()));
        Ok(ResponsePayload::Areas(areas))
    }

    async fn select_area(&self, area_id: Option<AreaId>) -> DispatchResult {
        let _operation = self.reserve_idle("select an area").await?;
        if let Some(id) = &area_id
            && !self
                .inner
                .state
                .lock()
                .await
                .areas
                .iter()
                .any(|area| area.id == *id)
        {
            return Err(ActionableError::new(
                ErrorCode::AreaNotFound,
                "The selected area was not found",
            )
            .with_action("Refresh the Entertainment area list"));
        }
        self.update_config_reserved(ConfigUpdate {
            selected_area: Some(area_id),
            ..ConfigUpdate::default()
        })
        .await
    }

    async fn update_config(&self, update: ConfigUpdate) -> DispatchResult {
        if matches!(update.bridge, Some(None)) {
            return self.forget_bridge().await;
        }
        let sync_only = update.sync.is_some()
            && update.language.is_none()
            && update.capture_backend.is_none()
            && update.restore_on_stop.is_none()
            && update.launch_at_login.is_none()
            && update.auto_start_sync.is_none()
            && update.selected_display.is_none()
            && update.selected_area.is_none()
            && update.bridge.is_none();
        let _operation = if sync_only {
            self.reserve_sync_settings_update().await?
        } else {
            self.reserve_idle("update configuration").await?
        };
        self.update_config_reserved(update).await
    }

    async fn update_config_reserved(&self, update: ConfigUpdate) -> DispatchResult {
        if let Some(Some(area_id)) = &update.selected_area
            && !self
                .inner
                .state
                .lock()
                .await
                .areas
                .iter()
                .any(|area| area.id == *area_id)
        {
            return Err(ActionableError::new(
                ErrorCode::AreaNotFound,
                "The selected area was not found",
            ));
        }
        let bridge_changed = update.bridge.is_some();
        let mut next = self.inner.state.lock().await.config.clone();
        next.apply_update(update)
            .map_err(|error| invalid_configuration(error.to_string()))?;
        validate_supported_config(&next)?;
        let config = self.persist_config(next).await?;
        if bridge_changed {
            let mut state = self.inner.state.lock().await;
            state.hue = None;
            state.areas.clear();
            state.status.bridge = if config.bridge.is_some() {
                BridgeState::Disconnected
            } else {
                BridgeState::Unconfigured
            };
            drop(state);
            self.emit(Event::AreasChanged(Vec::new()));
            self.notify_status().await;
            self.initialize_bridge().await;
        }
        Ok(ResponsePayload::Config(config))
    }

    async fn create_profile(&self, profile: Profile) -> DispatchResult {
        let _operation = self.reserve_idle("create a profile").await?;
        profile
            .validate()
            .map_err(|error| invalid_configuration(error.to_string()))?;
        validate_supported_settings(&profile.settings)?;
        let state = self.inner.state.lock().await;
        if state
            .config
            .profiles
            .iter()
            .any(|item| item.id == profile.id)
        {
            return Err(conflict("A profile with this ID already exists"));
        }
        let mut next = state.config.clone();
        drop(state);
        next.profiles.push(profile.clone());
        next.revision = next.revision.saturating_add(1);
        next.validate()
            .map_err(|error| invalid_configuration(error.to_string()))?;
        self.persist_config(next).await?;
        Ok(ResponsePayload::Profile(profile))
    }

    async fn update_profile(&self, profile: Profile) -> DispatchResult {
        let _operation = self.reserve_idle("update a profile").await?;
        profile
            .validate()
            .map_err(|error| invalid_configuration(error.to_string()))?;
        validate_supported_settings(&profile.settings)?;
        let mut next = self.inner.state.lock().await.config.clone();
        let target = next
            .profiles
            .iter_mut()
            .find(|item| item.id == profile.id)
            .ok_or_else(|| ActionableError::new(ErrorCode::InvalidRequest, "Profile not found"))?;
        *target = profile.clone();
        next.revision = next.revision.saturating_add(1);
        next.validate()
            .map_err(|error| invalid_configuration(error.to_string()))?;
        self.persist_config(next).await?;
        Ok(ResponsePayload::Profile(profile))
    }

    async fn delete_profile(&self, profile_id: ProfileId) -> DispatchResult {
        let _operation = self.reserve_idle("delete a profile").await?;
        let mut next = self.inner.state.lock().await.config.clone();
        let length = next.profiles.len();
        next.profiles.retain(|profile| profile.id != profile_id);
        if next.profiles.len() == length {
            return Err(ActionableError::new(
                ErrorCode::InvalidRequest,
                "Profile not found",
            ));
        }
        if next.active_profile == Some(profile_id) {
            next.active_profile = None;
        }
        next.revision = next.revision.saturating_add(1);
        self.persist_config(next).await?;
        Ok(ResponsePayload::Acknowledged)
    }

    async fn activate_profile(&self, profile_id: Option<ProfileId>) -> DispatchResult {
        let _operation = self.reserve_idle("activate a profile").await?;
        let mut next = self.inner.state.lock().await.config.clone();
        if let Some(id) = profile_id {
            let profile = next
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
                .ok_or_else(|| {
                    ActionableError::new(ErrorCode::InvalidRequest, "Profile not found")
                })?;
            next.sync = profile.settings;
            next.selected_display = profile.display;
            next.selected_area = profile.area;
        }
        next.active_profile = profile_id;
        next.revision = next.revision.saturating_add(1);
        validate_supported_config(&next)?;
        let config = self.persist_config(next).await?;
        Ok(ResponsePayload::Config(config))
    }

    async fn persist_config(&self, next: AppConfig) -> Result<AppConfig, ActionableError> {
        config_store::save(&self.inner.config_path, &next)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to persist configuration");
                ActionableError::new(ErrorCode::Internal, "Could not save configuration")
                    .retryable(true)
            })?;
        self.inner.state.lock().await.config = next.clone();
        self.emit(Event::ConfigChanged {
            revision: next.revision,
        });
        Ok(next)
    }

    async fn start(&self, interactive: bool) -> DispatchResult {
        let _operation = self.reserve_sync_idle("start synchronization").await?;
        let (startup_cancel, mut cancelled) = watch::channel(false);
        let (config, client, area) = {
            let mut state = self.inner.state.lock().await;
            if !matches!(state.status.sync, SyncState::Stopped | SyncState::Failed) {
                return Err(conflict(
                    "Synchronization is already active or changing state",
                ));
            }
            let client = state.hue.clone().ok_or_else(|| {
                ActionableError::new(
                    ErrorCode::BridgeUnavailable,
                    "The Hue bridge is not connected",
                )
                .with_action("Pair or reconnect the bridge")
            })?;
            validate_supported_config(&state.config)?;
            let area_id = state.config.selected_area.clone().ok_or_else(|| {
                ActionableError::new(ErrorCode::AreaNotFound, "No Entertainment area is selected")
                    .with_action("Select an Entertainment area")
            })?;
            let area = state
                .areas
                .iter()
                .find(|item| item.id == area_id)
                .cloned()
                .ok_or_else(|| {
                    ActionableError::new(
                        ErrorCode::AreaNotFound,
                        "Refresh and select an Entertainment area",
                    )
                })?;
            state.status.sync = SyncState::Starting;
            state.status.capture = if state.config.capture_backend == CaptureBackend::PortalPipewire
            {
                CaptureState::RequestingPermission
            } else {
                CaptureState::Idle
            };
            state.status.error = None;
            state.startup_cancel = Some(startup_cancel);
            (state.config.clone(), client, area)
        };
        self.notify_status().await;

        let capture: Result<Box<dyn Capture>, CaptureError> = match config.capture_backend {
            CaptureBackend::PortalPipewire => tokio::select! {
                result = PortalCapture::start(PortalOptions {
                    interactive,
                    restore_token: None,
                    restore_token_path: Some(self.inner.portal_token_path.clone()),
                }) => result.map(|capture| Box::new(capture) as Box<dyn Capture>),
                _ = cancelled.changed() => Err(CaptureError::Closed),
            },
            CaptureBackend::Grim => Ok(Box::new(GrimCapture::new(
                config
                    .selected_display
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
            ))),
        };
        let mut capture = match capture {
            Ok(capture) => capture,
            Err(CaptureError::Closed) if *cancelled.borrow() => {
                return Err(self.start_cancelled().await);
            }
            Err(error) => return Err(self.start_failed(classify_capture(&error)).await),
        };
        let first_frame = tokio::select! {
            result = tokio::time::timeout(FIRST_FRAME_TIMEOUT, capture.next_frame()) => {
                result.unwrap_or_else(|_| Err(CaptureError::Backend(
                    "screen capture timed out waiting for its first frame".to_owned()
                )))
            },
            _ = cancelled.changed() => Err(CaptureError::Closed),
        };
        if let Err(error) = first_frame {
            let _ = capture.shutdown().await;
            if *cancelled.borrow() {
                return Err(self.start_cancelled().await);
            }
            return Err(self.start_failed(classify_capture(&error)).await);
        }
        let stream = match client
            .open_stream_with_restore(area.id.as_str(), config.restore_on_stop)
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(error = %error, "could not start Hue stream");
                let _ = tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, capture.shutdown()).await;
                return Err(self
                    .start_failed(classify_hue("Could not start Hue Entertainment streaming"))
                    .await);
            }
        };
        if *cancelled.borrow() {
            let _ = tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, capture.shutdown()).await;
            if let Ok(Err(error)) =
                tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, stream.stop()).await
            {
                tracing::warn!(error = %error, "Hue cleanup failed after startup cancellation");
            }
            return Err(self.start_cancelled().await);
        }
        let (stop, stopped) = oneshot::channel();
        let (release, start_task) = oneshot::channel();
        let daemon = self.clone();
        let task = tokio::spawn(async move {
            let _ = start_task.await;
            daemon
                .run_sync(capture, Box::new(stream), area, stopped)
                .await;
        });
        {
            let mut state = self.inner.state.lock().await;
            state.startup_cancel = None;
            state.sync = Some(SyncControl { stop, task });
            state.status.capture = CaptureState::Capturing;
            state.status.sync = SyncState::Running;
            state.status.active_area = state.config.selected_area.clone();
            state.status.frames_per_second = Some(0.0);
        }
        self.notify_status().await;
        let _ = release.send(());
        Ok(ResponsePayload::Acknowledged)
    }

    async fn run_sync(
        &self,
        mut capture: Box<dyn Capture>,
        stream: Box<dyn EntertainmentStream>,
        area: EntertainmentArea,
        mut stop: oneshot::Receiver<()>,
    ) {
        let result = self
            .sync_frames(&mut *capture, &*stream, &area, &mut stop)
            .await;
        let capture_result = tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, capture.shutdown())
            .await
            .unwrap_or(Err(CaptureError::ShutdownTimedOut));
        let stream_result = tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, stream.stop())
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Hue cleanup timed out")));
        if let Err(error) = &capture_result {
            tracing::warn!(error = %error, "capture cleanup failed");
        }
        if let Err(error) = &stream_result {
            tracing::warn!(error = %error, "Hue cleanup or light restoration failed");
        }
        let failure = result
            .err()
            .or_else(|| capture_result.err().map(|error| classify_capture(&error)))
            .or_else(|| {
                stream_result.err().map(|_| {
                    ActionableError::new(
                        ErrorCode::SyncUnavailable,
                        "Hue cleanup or light restoration failed",
                    )
                    .retryable(true)
                })
            });
        let mut state = self.inner.state.lock().await;
        state.sync = None;
        state.status.capture = CaptureState::Idle;
        state.status.active_area = None;
        state.status.frames_per_second = None;
        state.status.sync = if failure.is_some() {
            SyncState::Failed
        } else {
            SyncState::Stopped
        };
        state.status.error = failure;
        drop(state);
        self.notify_status().await;
    }

    async fn sync_frames(
        &self,
        capture: &mut dyn Capture,
        stream: &dyn EntertainmentStream,
        area: &EntertainmentArea,
        stop: &mut oneshot::Receiver<()>,
    ) -> Result<(), ActionableError> {
        let mut processor = ColorProcessor::new(area.channels.clone())
            .map_err(|_| invalid_configuration("The selected area has invalid channels"))?;
        let mut cadence = tokio::time::interval(Duration::from_millis(1_000 / TARGET_FPS));
        cadence.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut frames = 0_u32;
        let mut measured_at = Instant::now();
        loop {
            let frame = tokio::select! {
                _ = &mut *stop => return Ok(()),
                result = capture.next_frame() => result.map_err(|error| classify_capture(&error))?,
            };
            cadence.tick().await;
            let format = frame.format();
            let pixel_format = map_pixel_format(format.pixel_format);
            let stride = if format.stride == 0 {
                i32::try_from(
                    u64::from(format.width) * format.pixel_format.bytes_per_pixel() as u64,
                )
                .map_err(|_| {
                    ActionableError::new(ErrorCode::CaptureUnavailable, "Capture frame is too wide")
                })?
            } else {
                format.stride
            };
            let view = FrameView::new(
                frame.data(),
                format.width as usize,
                format.height as usize,
                stride,
                pixel_format,
            )
            .map_err(|_| {
                ActionableError::new(
                    ErrorCode::CaptureUnavailable,
                    "Capture returned an invalid frame",
                )
            })?;
            let settings = self.inner.state.lock().await.config.sync.clone();
            let colors = processor
                .process(
                    &view,
                    settings.mode,
                    settings.intensity,
                    settings.brightness,
                )
                .map_err(|_| {
                    ActionableError::new(ErrorCode::SyncUnavailable, "Color processing failed")
                })?
                .into_iter()
                .map(|color| lightsync_hue::ChannelColor {
                    channel_id: u8::try_from(color.channel_id).unwrap_or(u8::MAX),
                    red: color.color.red,
                    green: color.color.green,
                    blue: color.color.blue,
                })
                .collect();
            stream.send(colors).map_err(|error| {
                tracing::warn!(error = %error, "Hue frame delivery failed");
                ActionableError::new(
                    ErrorCode::SyncUnavailable,
                    "Hue Entertainment stream stopped",
                )
                .retryable(true)
            })?;
            frames = frames.saturating_add(1);
            let elapsed = measured_at.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let fps = frames as f32 / elapsed.as_secs_f32();
                let mut state = self.inner.state.lock().await;
                state.status.frames_per_second = Some(fps);
                drop(state);
                self.notify_status().await;
                frames = 0;
                measured_at = Instant::now();
            }
        }
    }

    async fn stop(&self) -> DispatchResult {
        let startup = self.inner.state.lock().await.startup_cancel.clone();
        if let Some(cancel) = startup {
            let _ = cancel.send(true);
            tokio::time::timeout(RESOURCE_CLEANUP_TIMEOUT, async {
                loop {
                    if self.inner.state.lock().await.status.sync != SyncState::Starting {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .map_err(|_| conflict("Synchronization startup did not cancel in time"))?;
        }
        let control = {
            let mut state = self.inner.state.lock().await;
            let Some(control) = state.sync.take() else {
                if state.status.sync == SyncState::Stopped {
                    return Ok(ResponsePayload::Acknowledged);
                }
                if state.status.sync == SyncState::Stopping {
                    drop(state);
                    let mut statuses = self.subscribe_status();
                    tokio::time::timeout(STOP_TIMEOUT, async {
                        loop {
                            if matches!(
                                statuses.borrow_and_update().sync,
                                SyncState::Stopped | SyncState::Failed
                            ) {
                                break;
                            }
                            statuses.changed().await.map_err(|_| {
                                conflict("Synchronization status stream closed during cleanup")
                            })?;
                        }
                        Ok::<(), ActionableError>(())
                    })
                    .await
                    .map_err(|_| conflict("Synchronization cleanup did not finish in time"))??;
                    return Ok(ResponsePayload::Acknowledged);
                }
                return Err(conflict("Synchronization is not in a stoppable state"));
            };
            state.status.sync = SyncState::Stopping;
            control
        };
        self.notify_status().await;
        let SyncControl { stop, mut task } = control;
        let _ = stop.send(());
        let joined = tokio::time::timeout(STOP_TIMEOUT, &mut task).await;
        let result = match joined {
            Ok(result) => result,
            Err(_) => {
                task.abort();
                return Err(ActionableError::new(
                    ErrorCode::Internal,
                    "Synchronization did not stop within the cleanup deadline",
                ));
            }
        };
        if let Err(error) = result {
            tracing::error!(error = %error, "synchronization task failed");
            return Err(ActionableError::new(
                ErrorCode::Internal,
                "Synchronization task failed",
            ));
        }
        Ok(ResponsePayload::Acknowledged)
    }

    pub async fn shutdown(&self) {
        {
            let mut state = self.inner.state.lock().await;
            state.status.service = ServiceState::Stopping;
        }
        self.notify_status().await;
        let _ = self.stop().await;
    }

    async fn start_failed(&self, error: ActionableError) -> ActionableError {
        let mut state = self.inner.state.lock().await;
        state.startup_cancel = None;
        state.status.capture = CaptureState::Failed;
        state.status.sync = SyncState::Failed;
        state.status.error = Some(error.clone());
        drop(state);
        self.notify_status().await;
        error
    }

    async fn start_cancelled(&self) -> ActionableError {
        let mut state = self.inner.state.lock().await;
        state.startup_cancel = None;
        state.status.capture = CaptureState::Idle;
        state.status.sync = SyncState::Stopped;
        state.status.error = None;
        drop(state);
        self.notify_status().await;
        conflict("Synchronization startup was cancelled")
    }

    async fn hue_client(&self) -> Result<HueClient, ActionableError> {
        self.inner.state.lock().await.hue.clone().ok_or_else(|| {
            ActionableError::new(
                ErrorCode::BridgeUnavailable,
                "The Hue bridge is not connected",
            )
            .with_action("Pair or reconnect the bridge")
        })
    }

    async fn reserve_idle(&self, operation: &str) -> Result<OperationGuard, ActionableError> {
        let guard = self.reserve_operation()?;
        let state = self.inner.state.lock().await;
        if !matches!(state.status.sync, SyncState::Stopped | SyncState::Failed) {
            Err(conflict(format!(
                "Cannot {operation} while synchronization is active"
            )))
        } else if matches!(
            state.status.bridge,
            BridgeState::Discovering | BridgeState::Pairing
        ) {
            Err(conflict("Another bridge operation is in progress"))
        } else {
            Ok(guard)
        }
    }

    async fn reserve_sync_idle(&self, operation: &str) -> Result<OperationGuard, ActionableError> {
        let guard = self.reserve_operation()?;
        let state = self.inner.state.lock().await;
        if matches!(state.status.sync, SyncState::Stopped | SyncState::Failed) {
            Ok(guard)
        } else {
            Err(conflict(format!(
                "Cannot {operation} while synchronization is active"
            )))
        }
    }

    async fn reserve_sync_settings_update(&self) -> Result<OperationGuard, ActionableError> {
        let guard = self.reserve_operation()?;
        let state = self.inner.state.lock().await;
        if matches!(
            state.status.sync,
            SyncState::Stopped | SyncState::Failed | SyncState::Running
        ) {
            Ok(guard)
        } else {
            Err(conflict(
                "Cannot update synchronization settings while synchronization is changing state",
            ))
        }
    }

    fn reserve_operation(&self) -> Result<OperationGuard, ActionableError> {
        self.inner
            .operation_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| conflict("Another operation is already in progress"))?;
        Ok(OperationGuard {
            inner: Arc::clone(&self.inner),
        })
    }

    pub(crate) async fn set_bridge_state(
        &self,
        bridge: BridgeState,
        error: Option<ActionableError>,
    ) {
        let mut state = self.inner.state.lock().await;
        state.status.bridge = bridge;
        state.status.error = error;
        drop(state);
        self.notify_status().await;
    }

    async fn notify_status(&self) {
        let status = self.status().await;
        self.inner.statuses.send_replace(status.clone());
        self.emit(Event::StatusChanged(status));
    }

    fn emit(&self, event: Event) {
        let _ = self.inner.events.send(EventEnvelope::new(event));
    }
}

fn candidate_from_discovered(
    bridge: &DiscoveredBridge,
) -> Result<BridgeCandidate, ActionableError> {
    let address: IpAddr = bridge.host.parse().map_err(|_| {
        invalid_configuration("The Hue bridge host must be an IPv4 or IPv6 address")
    })?;
    Ok(BridgeCandidate {
        id: bridge.id.as_str().to_owned(),
        address,
        name: bridge.name.clone().unwrap_or_else(|| "Hue Bridge".into()),
    })
}

fn client_from_config(bridge: &BridgeConfiguration) -> anyhow::Result<HueClient> {
    let address = bridge.host.parse::<IpAddr>()?;
    HueClient::new(BridgeCandidate {
        id: bridge.id.as_str().to_owned(),
        address,
        name: bridge.name.clone().unwrap_or_else(|| "Hue Bridge".into()),
    })
}

fn map_pixel_format(format: CapturePixelFormat) -> PixelFormat {
    match format {
        CapturePixelFormat::Rgb => PixelFormat::Rgb8,
        CapturePixelFormat::Rgba => PixelFormat::Rgba8,
        CapturePixelFormat::Rgbx => PixelFormat::Rgbx8,
        CapturePixelFormat::Bgra => PixelFormat::Bgra8,
        CapturePixelFormat::Bgrx => PixelFormat::Bgrx8,
    }
}

fn validate_supported_settings(settings: &SyncSettings) -> Result<(), ActionableError> {
    if !matches!(settings.mode, SyncMode::Video | SyncMode::Game) {
        return Err(invalid_configuration(
            "Only video and game screen-sampling modes are supported",
        ));
    }
    if settings.audio_reactive {
        return Err(invalid_configuration(
            "Audio-reactive synchronization is not supported",
        ));
    }
    Ok(())
}

fn validate_supported_config(config: &AppConfig) -> Result<(), ActionableError> {
    validate_supported_settings(&config.sync)?;
    if config.capture_backend == CaptureBackend::PortalPipewire && config.selected_display.is_some()
    {
        return Err(invalid_configuration(
            "The screen-cast portal selects a display through its chooser and cannot use a display ID",
        ));
    }
    Ok(())
}

fn classify_capture(error: &CaptureError) -> ActionableError {
    match error {
        CaptureError::UserCancelled => ActionableError::new(
            ErrorCode::CapturePermissionDenied,
            "Screen capture was cancelled",
        )
        .with_action("Start synchronization again and select a display"),
        CaptureError::ConsentRequired => ActionableError::new(
            ErrorCode::CapturePermissionDenied,
            "Screen capture requires permission",
        )
        .with_action("Start synchronization interactively once to grant permission"),
        _ => ActionableError::new(ErrorCode::CaptureUnavailable, "Screen capture failed")
            .with_action("Check the selected capture backend")
            .retryable(true),
    }
}

fn classify_hue(message: impl Into<String>) -> ActionableError {
    ActionableError::new(ErrorCode::BridgeUnavailable, message)
        .with_action("Check the bridge connection and pairing")
        .retryable(true)
}

fn invalid_configuration(message: impl Into<String>) -> ActionableError {
    ActionableError::new(ErrorCode::InvalidConfiguration, message)
}

fn conflict(message: impl Into<String>) -> ActionableError {
    ActionableError::new(ErrorCode::Conflict, message).retryable(true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use lightsync_capture::OwnedFrame;
    use lightsync_domain::{Brightness, Intensity, SyncMode, SyncSettings};
    use lightsync_hue::fake::{
        MemoryCredentialStore, RecordingStreamConnector, ScriptedControlTransport,
    };
    use lightsync_hue::{BridgeCredentials, ControlResponse, CredentialStore, HueClient};

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    async fn daemon() -> Daemon {
        let path = std::env::temp_dir().join(format!(
            "lightsync-daemon-state-{}-{}/config.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Daemon::load(path).await.expect("daemon")
    }

    fn profile(name: &str) -> Profile {
        Profile {
            id: ProfileId::new(),
            name: name.into(),
            settings: SyncSettings {
                mode: SyncMode::Game,
                intensity: Intensity::High,
                brightness: Brightness::new(75).expect("brightness"),
                audio_reactive: false,
            },
            display: None,
            area: None,
        }
    }

    struct FailingCapture {
        stopped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Capture for FailingCapture {
        async fn next_frame(&mut self) -> Result<Arc<OwnedFrame>, CaptureError> {
            Err(CaptureError::Backend("injected capture failure".into()))
        }

        fn state(&self) -> lightsync_capture::CaptureState {
            lightsync_capture::CaptureState::Streaming
        }

        async fn shutdown(&mut self) -> Result<(), CaptureError> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeStream {
        stopped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl EntertainmentStream for FakeStream {
        fn send(&self, _colors: Vec<lightsync_hue::ChannelColor>) -> anyhow::Result<()> {
            Ok(())
        }

        async fn stop(self: Box<Self>) -> anyhow::Result<()> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_and_profile_crud_update_revisions() {
        let daemon = daemon().await;
        assert!(matches!(
            daemon.dispatch(Request::GetStatus).await,
            Ok(ResponsePayload::Status(_))
        ));
        let profile = profile("Games");
        daemon
            .dispatch(Request::CreateProfile {
                profile: profile.clone(),
            })
            .await
            .expect("create");
        daemon
            .dispatch(Request::ActivateProfile {
                profile_id: Some(profile.id),
            })
            .await
            .expect("activate");
        let config = match daemon.dispatch(Request::GetConfig).await.expect("config") {
            ResponsePayload::Config(config) => config,
            other => panic!("unexpected payload: {other:?}"),
        };
        assert_eq!(config.revision, 2);
        assert_eq!(config.sync, profile.settings);
        daemon
            .dispatch(Request::DeleteProfile {
                profile_id: profile.id,
            })
            .await
            .expect("delete");
        let config = daemon.inner.state.lock().await.config.clone();
        assert!(config.profiles.is_empty());
        assert_eq!(config.active_profile, None);
        assert_eq!(config.revision, 3);
    }

    #[tokio::test]
    async fn invalid_profile_is_atomic_and_status_notifications_are_broadcast() {
        let daemon = daemon().await;
        let mut events = daemon.subscribe();
        let invalid = profile("   ");
        assert!(
            daemon
                .dispatch(Request::CreateProfile { profile: invalid })
                .await
                .is_err()
        );
        assert_eq!(daemon.inner.state.lock().await.config.revision, 0);
        daemon.set_bridge_state(BridgeState::Connecting, None).await;
        let event = events.recv().await.expect("event");
        assert!(matches!(
            event.event,
            Event::StatusChanged(StatusSnapshot {
                bridge: BridgeState::Connecting,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn failed_preconditions_do_not_enter_starting_state() {
        let daemon = daemon().await;
        let error = daemon
            .dispatch(Request::Start)
            .await
            .expect_err("start fails");
        assert_eq!(error.code, ErrorCode::BridgeUnavailable);
        let status = daemon.status().await;
        assert_eq!(status.sync, SyncState::Stopped);
        assert_eq!(status.capture, CaptureState::Idle);
    }

    #[tokio::test]
    async fn waiting_for_link_button_is_successful_pairing_progress() {
        let daemon = daemon().await;
        let control = Arc::new(ScriptedControlTransport::default());
        control.push_response(ControlResponse {
            status: 200,
            body: serde_json::json!([{"error": {"type": 101}}]),
        });
        let client = HueClient::with_transports(
            BridgeCandidate {
                id: "001788fffe123456".into(),
                address: "192.0.2.1".parse().expect("address"),
                name: "Desk".into(),
            },
            control,
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(RecordingStreamConnector::default()),
        )
        .expect("client");
        let payload = daemon
            .pair_attempt(
                client,
                BridgeId::new("001788fffe123456").expect("bridge ID"),
            )
            .await
            .expect("waiting is progress, not an error");
        assert_eq!(payload, ResponsePayload::Acknowledged);
    }

    #[tokio::test]
    async fn idle_operations_are_reserved_atomically() {
        let daemon = daemon().await;
        let _first = daemon
            .reserve_idle("first")
            .await
            .expect("first reservation");
        let second = match daemon.reserve_idle("second").await {
            Ok(_) => panic!("second operation must conflict"),
            Err(error) => error,
        };
        assert_eq!(second.code, ErrorCode::Conflict);
    }

    #[tokio::test]
    async fn unsupported_modes_and_portal_display_ids_are_rejected_atomically() {
        let daemon = daemon().await;
        let settings = SyncSettings {
            mode: SyncMode::Music,
            ..SyncSettings::default()
        };
        let error = daemon
            .dispatch(Request::UpdateConfig {
                update: ConfigUpdate {
                    sync: Some(settings),
                    ..ConfigUpdate::default()
                },
            })
            .await
            .expect_err("music mode is unsupported");
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert_eq!(daemon.inner.state.lock().await.config.revision, 0);

        let error = daemon
            .dispatch(Request::UpdateConfig {
                update: ConfigUpdate {
                    selected_display: Some(Some(
                        lightsync_domain::DisplayId::new("display-1").expect("display ID"),
                    )),
                    ..ConfigUpdate::default()
                },
            })
            .await
            .expect_err("portal display targeting is unsupported");
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert_eq!(daemon.inner.state.lock().await.config.revision, 0);
    }

    #[tokio::test]
    async fn sync_settings_can_change_while_streaming() {
        let daemon = daemon().await;
        daemon.inner.state.lock().await.status.sync = SyncState::Running;
        let settings = SyncSettings {
            mode: SyncMode::Game,
            intensity: Intensity::Extreme,
            brightness: Brightness::new(42).expect("brightness"),
            audio_reactive: false,
        };
        daemon
            .dispatch(Request::UpdateConfig {
                update: ConfigUpdate {
                    sync: Some(settings.clone()),
                    ..ConfigUpdate::default()
                },
            })
            .await
            .expect("live settings update");
        assert_eq!(daemon.inner.state.lock().await.config.sync, settings);
    }

    #[tokio::test]
    async fn concurrent_shutdown_waits_for_the_active_cleanup() {
        let daemon = daemon().await;
        let cleanup_finished = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = oneshot::channel();
        let task_daemon = daemon.clone();
        let task_cleanup_finished = Arc::clone(&cleanup_finished);
        let task = tokio::spawn(async move {
            let _ = stopped.await;
            tokio::time::sleep(Duration::from_millis(25)).await;
            task_cleanup_finished.store(true, Ordering::SeqCst);
            let mut state = task_daemon.inner.state.lock().await;
            state.status.sync = SyncState::Stopped;
            state.status.capture = CaptureState::Idle;
            drop(state);
            task_daemon.notify_status().await;
        });
        {
            let mut state = daemon.inner.state.lock().await;
            state.status.sync = SyncState::Running;
            state.status.capture = CaptureState::Capturing;
            state.sync = Some(SyncControl { stop, task });
        }

        let stop_daemon = daemon.clone();
        let shutdown_daemon = daemon.clone();
        let (stop_result, ()) = tokio::join!(stop_daemon.stop(), shutdown_daemon.shutdown());
        stop_result.expect("stop");
        assert!(cleanup_finished.load(Ordering::SeqCst));
        assert_eq!(
            daemon.inner.state.lock().await.status.sync,
            SyncState::Stopped
        );
    }

    #[tokio::test]
    async fn pending_pairing_candidate_survives_reload_without_credentials() {
        let path = std::env::temp_dir().join(format!(
            "lightsync-daemon-pairing-{}-{}/config.toml",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let config = AppConfig::default();
        let pending = BridgeConfiguration {
            id: BridgeId::new("001788fffe123456").expect("bridge ID"),
            host: "192.0.2.1".into(),
            name: Some("Desk".into()),
        };
        config_store::save(&path, &config).await.expect("save");
        config_store::save_pending_pairing(&path, Some(&pending))
            .await
            .expect("save pending pairing");
        let daemon = Daemon::load(path).await.expect("reload");
        let state = daemon.inner.state.lock().await;
        assert!(state.pending_pairing.is_some());
        assert_eq!(state.status.bridge, BridgeState::Pairing);
    }

    #[tokio::test]
    async fn forget_bridge_deletes_credentials_and_clears_area_configuration() {
        let daemon = daemon().await;
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials
            .store(
                "001788fffe123456",
                &BridgeCredentials::new("application-key".into(), "0011".into())
                    .expect("credentials"),
            )
            .await
            .expect("store credentials");
        let client = HueClient::with_transports(
            BridgeCandidate {
                id: "001788fffe123456".into(),
                address: "192.0.2.1".parse().expect("address"),
                name: "Desk".into(),
            },
            Arc::new(ScriptedControlTransport::default()),
            credentials.clone(),
            Arc::new(RecordingStreamConnector::default()),
        )
        .expect("client");
        {
            let mut state = daemon.inner.state.lock().await;
            state.config.bridge = Some(BridgeConfiguration {
                id: BridgeId::new("001788fffe123456").expect("bridge ID"),
                host: "192.0.2.1".into(),
                name: Some("Desk".into()),
            });
            state.config.selected_area = Some(AreaId::new("area").expect("area ID"));
            state.hue = Some(client);
        }
        daemon
            .dispatch(Request::ForgetBridge)
            .await
            .expect("forget bridge");
        assert!(credentials.load("001788fffe123456").await.is_err());
        let state = daemon.inner.state.lock().await;
        assert!(state.config.bridge.is_none());
        assert!(state.config.selected_area.is_none());
        assert_eq!(state.status.bridge, BridgeState::Unconfigured);
    }

    #[tokio::test]
    async fn capture_failure_stops_both_owned_resources() {
        let daemon = daemon().await;
        let capture_stopped = Arc::new(AtomicBool::new(false));
        let stream_stopped = Arc::new(AtomicBool::new(false));
        let (_stop, stopped) = oneshot::channel();
        daemon
            .run_sync(
                Box::new(FailingCapture {
                    stopped: Arc::clone(&capture_stopped),
                }),
                Box::new(FakeStream {
                    stopped: Arc::clone(&stream_stopped),
                }),
                EntertainmentArea {
                    id: AreaId::new("area").expect("area ID"),
                    name: "Area".into(),
                    channels: Vec::new(),
                },
                stopped,
            )
            .await;
        assert!(capture_stopped.load(Ordering::SeqCst));
        assert!(stream_stopped.load(Ordering::SeqCst));
        assert_eq!(daemon.status().await.sync, SyncState::Failed);
    }

    #[test]
    fn maps_hue_coordinates_and_all_capture_formats() {
        let point = NormalizedPoint::new(
            ((-1.0_f32 + 1.0) / 2.0).clamp(0.0, 1.0),
            ((1.0_f32 - 1.0) / 2.0).clamp(0.0, 1.0),
        )
        .expect("point");
        assert_eq!((point.x(), point.y()), (0.0, 0.0));
        assert_eq!(map_pixel_format(CapturePixelFormat::Rgb), PixelFormat::Rgb8);
        assert_eq!(
            map_pixel_format(CapturePixelFormat::Bgrx),
            PixelFormat::Bgrx8
        );
        assert_eq!(
            map_pixel_format(CapturePixelFormat::Bgra),
            PixelFormat::Bgra8
        );
        assert_eq!(
            map_pixel_format(CapturePixelFormat::Rgbx),
            PixelFormat::Rgbx8
        );
        assert_eq!(
            map_pixel_format(CapturePixelFormat::Rgba),
            PixelFormat::Rgba8
        );
    }
}
