mod i18n;
mod view_state;
mod worker;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use lightsync_domain::{
    ActionableError, AppConfig, BridgeId, BridgeState, Brightness, Capabilities, CaptureBackend,
    ConfigUpdate, DiscoveredBridge, EntertainmentArea, Intensity, Language, Profile, ProfileId,
    Request, ResponsePayload, StatusSnapshot, SyncMode, SyncSettings,
};

use crate::i18n::I18n;
use crate::view_state::{bridge_id, capture_id, dashboard_state, service_id, sync_id};
use crate::worker::{Worker, WorkerEvent};

const APPLICATION_ID: &str = "io.github.mahype.omarchylightsync";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Operation {
    GetConfig,
    GetCapabilities,
    Discover,
    BeginPairing,
    CompletePairing,
    RefreshAreas,
    SelectArea,
    UpdateSync,
    UpdateConfig,
    CreateProfile,
    DeleteProfile,
    ActivateProfile,
    Start,
    Stop,
}

fn operation(request: &Request) -> Option<Operation> {
    Some(match request {
        Request::GetConfig => Operation::GetConfig,
        Request::GetCapabilities => Operation::GetCapabilities,
        Request::DiscoverBridges => Operation::Discover,
        Request::BeginPairing { .. } => Operation::BeginPairing,
        Request::CompletePairing { .. } => Operation::CompletePairing,
        Request::RefreshAreas => Operation::RefreshAreas,
        Request::SelectArea { .. } => Operation::SelectArea,
        Request::UpdateConfig { update } if update.sync.is_some() => Operation::UpdateSync,
        Request::UpdateConfig { .. } => Operation::UpdateConfig,
        Request::CreateProfile { .. } => Operation::CreateProfile,
        Request::DeleteProfile { .. } => Operation::DeleteProfile,
        Request::ActivateProfile { .. } => Operation::ActivateProfile,
        Request::Start => Operation::Start,
        Request::Stop => Operation::Stop,
        Request::GetStatus
        | Request::ForgetBridge
        | Request::UpdateProfile { .. }
        | Request::Toggle
        | Request::WatchStatus { .. } => return None,
    })
}

fn should_refresh_areas(status: &StatusSnapshot) -> bool {
    status.bridge == BridgeState::Connected
}

#[derive(Default)]
struct Model {
    connected: bool,
    status: Option<StatusSnapshot>,
    config: Option<AppConfig>,
    capabilities: Option<Capabilities>,
    bridges: Vec<DiscoveredBridge>,
    areas: Vec<EntertainmentArea>,
    operation_error: Option<ActionableError>,
    pending: HashMap<Operation, u64>,
    pairing_bridge: Option<BridgeId>,
    optimistic_sync: Option<SyncSettings>,
    bridges_revision: u64,
    areas_revision: u64,
    profiles_revision: u64,
}

struct Ui {
    service: gtk::Label,
    bridge: gtk::Label,
    area: gtk::Label,
    error: gtk::Label,
    action: gtk::Button,
    mode: [gtk::ToggleButton; 4],
    brightness: gtk::Scale,
    intensity: gtk::DropDown,
    bridges: gtk::ListBox,
    areas: gtk::ListBox,
    profiles: gtk::ListBox,
    capture_backend: gtk::DropDown,
    restore: gtk::Switch,
    auto_start: gtk::Switch,
    language: gtk::DropDown,
    discover: gtk::Button,
    complete_pairing: gtk::Button,
    refresh_areas: gtk::Button,
    create_profile: gtk::Button,
    profile_name: gtk::Entry,
    capture_backends: Vec<CaptureBackend>,
    bridges_revision: Cell<u64>,
    areas_revision: Cell<u64>,
    profiles_revision: Cell<u64>,
    bridge_actions: RefCell<Vec<gtk::Button>>,
    area_actions: RefCell<Vec<(gtk::Button, bool)>>,
    profile_actions: RefCell<Vec<(gtk::Button, gtk::Button, bool)>>,
    diagnostic_service: gtk::Label,
    diagnostic_bridge: gtk::Label,
    diagnostic_capture: gtk::Label,
    diagnostic_sync: gtk::Label,
    diagnostic_fps: gtk::Label,
}

struct AppInner {
    window: adw::ApplicationWindow,
    worker: Worker,
    model: RefCell<Model>,
    ui: RefCell<Option<Ui>>,
    applying: Cell<bool>,
    brightness_debounce: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
struct App(Rc<AppInner>);

impl App {
    fn new(application: &adw::Application, worker: Worker) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .default_width(960)
            .default_height(700)
            .width_request(480)
            .height_request(620)
            .build();
        let app = Self(Rc::new(AppInner {
            window,
            worker,
            model: RefCell::new(Model::default()),
            ui: RefCell::new(None),
            applying: Cell::new(false),
            brightness_debounce: RefCell::new(None),
        }));
        app.rebuild();
        app.0.window.present();
        app
    }

    fn i18n(&self) -> I18n {
        let language = self
            .0
            .model
            .borrow()
            .config
            .as_ref()
            .map_or(Language::System, |config| config.language);
        I18n::new(language)
    }

    fn send(&self, request: Request) -> bool {
        let Some(kind) = operation(&request) else {
            self.0.worker.send(request);
            return true;
        };
        let mut model = self.0.model.borrow_mut();
        if !model.connected {
            return false;
        }
        if model.pending.contains_key(&kind) {
            return false;
        }
        model.operation_error = None;
        let id = self.0.worker.send(request);
        model.pending.insert(kind, id);
        drop(model);
        self.render();
        true
    }

    fn update_config(&self, update: ConfigUpdate) {
        if !self.0.applying.get() {
            self.send(Request::UpdateConfig { update });
        }
    }

    fn update_sync(&self, change: impl FnOnce(&mut SyncSettings), debounce: bool) {
        if self.0.applying.get() {
            return;
        }
        {
            let mut model = self.0.model.borrow_mut();
            let Some(mut settings) = model
                .optimistic_sync
                .clone()
                .or_else(|| model.config.as_ref().map(|config| config.sync.clone()))
            else {
                return;
            };
            change(&mut settings);
            model.optimistic_sync = Some(settings);
        }
        if let Some(source) = self.0.brightness_debounce.borrow_mut().take() {
            source.remove();
        }
        if debounce {
            let app = self.clone();
            let source = glib::timeout_add_local_once(Duration::from_millis(120), move || {
                app.0.brightness_debounce.borrow_mut().take();
                app.flush_sync();
            });
            *self.0.brightness_debounce.borrow_mut() = Some(source);
        } else {
            self.flush_sync();
        }
    }

    fn flush_sync(&self) {
        let settings = self.0.model.borrow().optimistic_sync.clone();
        if let Some(settings) = settings {
            self.send(Request::UpdateConfig {
                update: ConfigUpdate {
                    sync: Some(settings),
                    ..ConfigUpdate::default()
                },
            });
        }
    }

    fn rebuild(&self) {
        let i18n = self.i18n();
        self.0.window.set_title(Some(&i18n.text("app-name")));
        let (root, ui) = build_ui(self, &i18n);
        self.0.window.set_content(Some(&root));
        *self.0.ui.borrow_mut() = Some(ui);
        self.render();
    }

    fn render(&self) {
        let i18n = self.i18n();
        let model = self.0.model.borrow();
        let ui_ref = self.0.ui.borrow();
        let Some(ui) = ui_ref.as_ref() else { return };
        self.0.applying.set(true);

        let state = dashboard_state(model.connected, model.status.as_ref());
        let old_service = ui.service.label();
        ui.service.set_label(&i18n.text(state.service_id));
        if old_service != ui.service.label() {
            ui.service.announce(
                &ui.service.label(),
                gtk::AccessibleAnnouncementPriority::Low,
            );
        }
        ui.bridge.set_label(&i18n.text(state.bridge_id));
        ui.action.set_label(&i18n.text(state.action_id));
        let configured = model
            .config
            .as_ref()
            .is_some_and(|config| config.bridge.is_some() && config.selected_area.is_some());
        let selection_supported = model.config.as_ref().is_some_and(|config| {
            let sync = model.optimistic_sync.as_ref().unwrap_or(&config.sync);
            model.capabilities.as_ref().is_some_and(|capabilities| {
                capabilities.sync_modes.contains(&sync.mode)
                    && capabilities
                        .capture_backends
                        .contains(&config.capture_backend)
                    && (sync.mode != SyncMode::Music || capabilities.audio_reactive)
            })
        });
        let action_pending = model.pending.contains_key(if state.action_is_stop {
            &Operation::Stop
        } else {
            &Operation::Start
        });
        ui.action.set_sensitive(
            state.action_enabled
                && model.connected
                && (state.action_is_stop || (configured && selection_supported))
                && !action_pending,
        );
        if action_pending {
            ui.action.set_label(&i18n.text("action-working"));
        }
        ui.action.set_css_classes(if state.action_is_stop {
            &["destructive-action", "pill"]
        } else {
            &["suggested-action", "pill"]
        });
        let area_name = model
            .status
            .as_ref()
            .and_then(|status| status.active_area.as_ref())
            .and_then(|active| model.areas.iter().find(|area| &area.id == active))
            .map_or_else(|| i18n.text("area-none"), |area| area.name.clone());
        ui.area.set_label(&area_name);
        let error_id = model
            .operation_error
            .as_ref()
            .map(|error| crate::view_state::error_id(error.code))
            .or(state.error_id);
        let old_error = ui.error.label();
        ui.error
            .set_label(&error_id.map_or_else(String::new, |id| i18n.text(id)));
        ui.error.set_visible(error_id.is_some());
        if error_id.is_some() && old_error != ui.error.label() {
            ui.error
                .announce(&ui.error.label(), gtk::AccessibleAnnouncementPriority::High);
        }

        if let Some(config) = &model.config {
            let sync = model.optimistic_sync.as_ref().unwrap_or(&config.sync);
            let mode_index = match sync.mode {
                SyncMode::Video => 0,
                SyncMode::Game => 1,
                SyncMode::Music => 2,
                SyncMode::Scene => 3,
            };
            ui.mode[mode_index].set_active(true);
            ui.brightness.set_value(f64::from(sync.brightness.get()));
            ui.intensity.set_selected(match sync.intensity {
                Intensity::Subtle => 0,
                Intensity::Moderate => 1,
                Intensity::High => 2,
                Intensity::Extreme => 3,
            });
            if let Some(index) = ui
                .capture_backends
                .iter()
                .position(|backend| *backend == config.capture_backend)
            {
                ui.capture_backend.set_selected(index as u32);
            }
            ui.restore.set_active(config.restore_on_stop);
            ui.auto_start.set_active(config.auto_start_sync);
            ui.language.set_selected(match config.language {
                Language::System => 0,
                Language::En => 1,
                Language::De => 2,
            });
        }
        let controls_enabled = model.connected && model.config.is_some();
        let capabilities = model.capabilities.as_ref();
        for (button, mode) in ui.mode.iter().zip([
            SyncMode::Video,
            SyncMode::Game,
            SyncMode::Music,
            SyncMode::Scene,
        ]) {
            let supported = capabilities.is_some_and(|value| value.sync_modes.contains(&mode))
                && (mode != SyncMode::Music
                    || capabilities.is_some_and(|value| value.audio_reactive));
            button.set_sensitive(controls_enabled && supported);
            let id = match (mode, supported) {
                (SyncMode::Video, false) => "mode-video-unavailable",
                (SyncMode::Game, false) => "mode-game-unavailable",
                (SyncMode::Music, false) => "mode-music-unavailable",
                (SyncMode::Scene, false) => "mode-scene-unavailable",
                (SyncMode::Video, true) => "mode-video",
                (SyncMode::Game, true) => "mode-game",
                (SyncMode::Music, true) => "mode-music",
                (SyncMode::Scene, true) => "mode-scene",
            };
            button.set_label(&i18n.text(id));
        }
        ui.brightness.set_sensitive(controls_enabled);
        ui.intensity.set_sensitive(controls_enabled);
        let general_enabled = model.connected && model.config.is_some();
        ui.capture_backend.set_sensitive(
            general_enabled
                && ui.capture_backends.len() > 1
                && !model.pending.contains_key(&Operation::UpdateConfig),
        );
        for control in [&ui.restore, &ui.auto_start] {
            control.set_sensitive(
                general_enabled && !model.pending.contains_key(&Operation::UpdateConfig),
            );
        }
        ui.language.set_sensitive(
            general_enabled && !model.pending.contains_key(&Operation::UpdateConfig),
        );
        ui.discover.set_sensitive(
            model.connected
                && capabilities.is_some_and(|value| value.discovery)
                && !model.pending.contains_key(&Operation::Discover),
        );
        ui.discover.set_label(
            &i18n.text(if capabilities.is_some_and(|value| value.discovery) {
                "action-discover"
            } else {
                "action-discover-unavailable"
            }),
        );
        ui.complete_pairing.set_sensitive(
            model.connected
                && model.pairing_bridge.is_some()
                && !model.pending.contains_key(&Operation::CompletePairing),
        );
        let bridge_connected = model
            .status
            .as_ref()
            .is_some_and(|status| status.bridge == BridgeState::Connected);
        ui.refresh_areas.set_sensitive(
            model.connected
                && bridge_connected
                && !model.pending.contains_key(&Operation::RefreshAreas),
        );
        ui.create_profile.set_sensitive(
            general_enabled
                && !model.pending.contains_key(&Operation::CreateProfile)
                && !ui.profile_name.text().trim().is_empty(),
        );
        for button in ui.bridge_actions.borrow().iter() {
            button.set_sensitive(
                model.connected
                    && model.pairing_bridge.is_none()
                    && !model.pending.contains_key(&Operation::BeginPairing)
                    && capabilities.is_some_and(|value| value.discovery),
            );
        }
        for (button, selected) in ui.area_actions.borrow().iter() {
            button.set_sensitive(
                model.connected
                    && bridge_connected
                    && !selected
                    && !model.pending.contains_key(&Operation::SelectArea),
            );
        }
        for (activate, delete, active) in ui.profile_actions.borrow().iter() {
            activate.set_sensitive(
                model.connected
                    && !active
                    && !model.pending.contains_key(&Operation::ActivateProfile),
            );
            delete.set_sensitive(
                model.connected && !model.pending.contains_key(&Operation::DeleteProfile),
            );
        }

        if ui.bridges_revision.get() != model.bridges_revision {
            render_bridges(self, &i18n, ui, &model);
            ui.bridges_revision.set(model.bridges_revision);
        }
        if ui.areas_revision.get() != model.areas_revision {
            render_areas(self, &i18n, ui, &model);
            ui.areas_revision.set(model.areas_revision);
        }
        if ui.profiles_revision.get() != model.profiles_revision {
            render_profiles(self, &i18n, ui, &model);
            ui.profiles_revision.set(model.profiles_revision);
        }

        let status = model.status.as_ref();
        ui.diagnostic_service.set_label(
            &i18n.text(status.map_or("state-reconnecting", |status| service_id(status.service))),
        );
        ui.diagnostic_bridge.set_label(
            &i18n.text(status.map_or("state-unknown", |status| bridge_id(status.bridge))),
        );
        ui.diagnostic_capture.set_label(
            &i18n.text(status.map_or("state-unknown", |status| capture_id(status.capture))),
        );
        ui.diagnostic_sync
            .set_label(&i18n.text(status.map_or("state-unknown", |status| sync_id(status.sync))));
        ui.diagnostic_fps.set_label(
            &status
                .and_then(|status| status.frames_per_second)
                .map_or_else(|| i18n.text("fps-unavailable"), |fps| format!("{fps:.1}")),
        );
        self.0.applying.set(false);
    }

    fn handle(&self, event: WorkerEvent) {
        let mut rebuild = false;
        match event {
            WorkerEvent::Status(status) => {
                let mut model = self.0.model.borrow_mut();
                let reconnected = !model.connected;
                let refresh_areas = reconnected && should_refresh_areas(&status);
                model.connected = true;
                model.status = Some(status);
                drop(model);
                if reconnected {
                    self.send(Request::GetConfig);
                    self.send(Request::GetCapabilities);
                    if refresh_areas {
                        self.send(Request::RefreshAreas);
                    }
                }
            }
            WorkerEvent::Disconnected => {
                let mut model = self.0.model.borrow_mut();
                model.connected = false;
                model.status = None;
            }
            WorkerEvent::Response {
                id,
                request,
                result,
            } => {
                let kind = operation(&request);
                if let Some(kind) = kind {
                    let mut model = self.0.model.borrow_mut();
                    if model.pending.get(&kind) != Some(&id) {
                        return;
                    }
                    model.pending.remove(&kind);
                }
                match *result {
                    Ok(payload) => {
                        let mut model = self.0.model.borrow_mut();
                        model.connected = true;
                        model.operation_error = None;
                        match payload {
                            ResponsePayload::Status(status) => model.status = Some(status),
                            ResponsePayload::Config(config) => {
                                rebuild = model
                                    .config
                                    .as_ref()
                                    .is_some_and(|old| old.language != config.language);
                                if model.config.as_ref().is_none_or(|old| {
                                    old.profiles != config.profiles
                                        || old.active_profile != config.active_profile
                                }) {
                                    model.profiles_revision =
                                        model.profiles_revision.wrapping_add(1);
                                }
                                if model
                                    .config
                                    .as_ref()
                                    .is_none_or(|old| old.selected_area != config.selected_area)
                                {
                                    model.areas_revision = model.areas_revision.wrapping_add(1);
                                }
                                model.config = Some(config);
                            }
                            ResponsePayload::Capabilities(capabilities) => {
                                rebuild = model.capabilities.as_ref() != Some(&capabilities);
                                model.capabilities = Some(capabilities);
                            }
                            ResponsePayload::Bridges(bridges) => {
                                if model.bridges != bridges {
                                    model.bridges = bridges;
                                    model.bridges_revision = model.bridges_revision.wrapping_add(1);
                                }
                            }
                            ResponsePayload::Areas(areas) => {
                                if model.areas != areas {
                                    model.areas = areas;
                                    model.areas_revision = model.areas_revision.wrapping_add(1);
                                }
                            }
                            ResponsePayload::Profile(_) | ResponsePayload::Acknowledged => {}
                        }
                        if let Request::BeginPairing { bridge } = &request {
                            model.pairing_bridge = Some(bridge.id.clone());
                        }
                        if let Request::CompletePairing { .. } = &request {
                            model.pairing_bridge = None;
                        }
                        if let Request::UpdateConfig { update } = &request
                            && let Some(sent) = &update.sync
                            && let Some(config) = model.config.as_mut()
                        {
                            config.sync = sent.clone();
                        }
                        let sync_changed_while_pending =
                            matches!(kind, Some(Operation::UpdateSync))
                                && model.optimistic_sync.as_ref()
                                    != match &request {
                                        Request::UpdateConfig { update } => update.sync.as_ref(),
                                        _ => None,
                                    };
                        drop(model);
                        if sync_changed_while_pending {
                            self.flush_sync();
                        } else if matches!(
                            &request,
                            Request::UpdateConfig { update } if update.sync.is_none()
                        ) || matches!(
                            &request,
                            Request::CreateProfile { .. }
                                | Request::UpdateProfile { .. }
                                | Request::DeleteProfile { .. }
                                | Request::ActivateProfile { .. }
                                | Request::SelectArea { .. }
                                | Request::CompletePairing { .. }
                        ) {
                            self.send(Request::GetConfig);
                        }
                        if matches!(&request, Request::CompletePairing { .. }) {
                            self.send(Request::RefreshAreas);
                        }
                    }
                    Err(lightsync_ipc::Error::Remote(error)) => {
                        self.0.model.borrow_mut().operation_error = Some(error);
                    }
                    Err(_) => {
                        let mut model = self.0.model.borrow_mut();
                        model.connected = false;
                        model.status = None;
                    }
                }
            }
        }
        if rebuild {
            self.rebuild();
        } else {
            self.render();
        }
    }
}

fn build_ui(app: &App, i18n: &I18n) -> (gtk::Box, Ui) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    header.set_css_classes(&["atmospheric-header"]);
    header.set_margin_start(24);
    header.set_margin_end(24);
    header.set_margin_top(18);
    header.set_margin_bottom(18);
    let mark = gtk::Image::from_icon_name("display-brightness-symbolic");
    mark.set_css_classes(&["sync-mark"]);
    mark.set_accessible_role(gtk::AccessibleRole::Presentation);
    let brand = gtk::Box::new(gtk::Orientation::Vertical, 1);
    brand.set_hexpand(true);
    brand.set_halign(gtk::Align::Start);
    let title = gtk::Label::new(Some(&i18n.text("app-name")));
    title.set_css_classes(&["title-2"]);
    title.set_halign(gtk::Align::Start);
    let tagline = gtk::Label::new(Some(&i18n.text("app-tagline")));
    tagline.set_css_classes(&["dim-label"]);
    brand.append(&title);
    brand.append(&tagline);
    let service = gtk::Label::new(None);
    service.set_css_classes(&["status-chip"]);
    header.append(&mark);
    header.append(&brand);
    header.append(&service);
    root.append(&header);

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let dashboard = build_dashboard(app, i18n);
    let setup = build_setup(app, i18n);
    let profiles = build_profiles(app, i18n);
    let settings = build_settings(app, i18n);
    let diagnostics = build_diagnostics(i18n);
    stack.add_titled(&dashboard.page, Some("sync"), &i18n.text("nav-sync"));
    stack.add_titled(&setup.page, Some("setup"), &i18n.text("nav-setup"));
    stack.add_titled(&profiles.page, Some("profiles"), &i18n.text("nav-profiles"));
    stack.add_titled(&settings.page, Some("settings"), &i18n.text("nav-settings"));
    stack.add_titled(
        &diagnostics.page,
        Some("diagnostics"),
        &i18n.text("nav-diagnostics"),
    );

    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(168)
        .build();
    sidebar.set_css_classes(&["navigation-sidebar"]);
    let navigation_labels = [
        i18n.text("nav-sync"),
        i18n.text("nav-setup"),
        i18n.text("nav-profiles"),
        i18n.text("nav-settings"),
        i18n.text("nav-diagnostics"),
    ];
    let navigation_refs: Vec<&str> = navigation_labels.iter().map(String::as_str).collect();
    let compact_navigation = gtk::DropDown::from_strings(&navigation_refs);
    compact_navigation.set_visible(false);
    compact_navigation.set_valign(gtk::Align::Center);
    compact_navigation.set_tooltip_text(Some(&i18n.text("nav-sync")));
    header.insert_child_after(&compact_navigation, Some(&brand));
    {
        let stack = stack.clone();
        compact_navigation.connect_selected_notify(move |navigation| {
            let name = ["sync", "setup", "profiles", "settings", "diagnostics"]
                [navigation.selected().min(4) as usize];
            stack.set_visible_child_name(name);
        });
    }
    {
        let navigation = compact_navigation.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            let selected = match stack.visible_child_name().as_deref() {
                Some("setup") => 1,
                Some("profiles") => 2,
                Some("settings") => 3,
                Some("diagnostics") => 4,
                _ => 0,
            };
            navigation.set_selected(selected);
        });
    }
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        720.0,
        adw::LengthUnit::Sp,
    ));
    narrow.add_setter(&sidebar, "visible", Some(&false.to_value()));
    narrow.add_setter(&compact_navigation, "visible", Some(&true.to_value()));
    narrow.add_setter(
        &dashboard.mode_row,
        "orientation",
        Some(&gtk::Orientation::Vertical.to_value()),
    );
    narrow.add_setter(
        &profiles.create_row,
        "orientation",
        Some(&gtk::Orientation::Vertical.to_value()),
    );
    app.0.window.add_breakpoint(narrow);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&sidebar);
    content.append(&stack);
    root.append(&content);

    (
        root,
        Ui {
            service,
            bridge: dashboard.bridge,
            area: dashboard.area,
            error: dashboard.error,
            action: dashboard.action,
            mode: dashboard.mode,
            brightness: dashboard.brightness,
            intensity: dashboard.intensity,
            bridges: setup.bridges,
            areas: setup.areas,
            profiles: profiles.list,
            capture_backend: settings.capture_backend,
            restore: settings.restore,
            auto_start: settings.auto_start,
            language: settings.language,
            discover: setup.discover,
            complete_pairing: setup.complete,
            refresh_areas: setup.refresh,
            create_profile: profiles.create,
            profile_name: profiles.entry,
            capture_backends: settings.capture_backends,
            bridges_revision: Cell::new(u64::MAX),
            areas_revision: Cell::new(u64::MAX),
            profiles_revision: Cell::new(u64::MAX),
            bridge_actions: RefCell::new(Vec::new()),
            area_actions: RefCell::new(Vec::new()),
            profile_actions: RefCell::new(Vec::new()),
            diagnostic_service: diagnostics.service,
            diagnostic_bridge: diagnostics.bridge,
            diagnostic_capture: diagnostics.capture,
            diagnostic_sync: diagnostics.sync,
            diagnostic_fps: diagnostics.fps,
        },
    )
}

struct DashboardWidgets {
    page: gtk::ScrolledWindow,
    bridge: gtk::Label,
    area: gtk::Label,
    error: gtk::Label,
    action: gtk::Button,
    mode: [gtk::ToggleButton; 4],
    brightness: gtk::Scale,
    intensity: gtk::DropDown,
    mode_row: gtk::Box,
}

fn build_dashboard(app: &App, i18n: &I18n) -> DashboardWidgets {
    let content = page_content(i18n, "dashboard-title", "dashboard-subtitle");
    let connection = card();
    let bridge = value_row(&connection, i18n, "bridge-title");
    let area = value_row(&connection, i18n, "area-title");
    content.append(&connection);

    let controls = card();
    controls.append(&section_label(i18n, "sync-controls-title"));
    let mode_label = field_label(i18n, "mode-title");
    controls.append(&mode_label);
    let modes = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    modes.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        mode_label.upcast_ref()
    ])]);
    modes.set_homogeneous(true);
    let video = gtk::ToggleButton::with_label(&i18n.text("mode-video"));
    let game = gtk::ToggleButton::with_label(&i18n.text("mode-game"));
    let music = gtk::ToggleButton::with_label(&i18n.text("mode-music-unavailable"));
    let scene = gtk::ToggleButton::with_label(&i18n.text("mode-scene"));
    for button in [&game, &music, &scene] {
        button.set_group(Some(&video));
    }
    for button in [&video, &game, &music, &scene] {
        button.set_css_classes(&["mode-button"]);
        modes.append(button);
    }
    video.set_active(true);
    controls.append(&modes);
    let audio_note = gtk::Label::new(Some(&i18n.text("audio-unavailable")));
    audio_note.set_wrap(true);
    audio_note.set_halign(gtk::Align::Start);
    audio_note.set_css_classes(&["dim-label", "caption"]);
    controls.append(&audio_note);
    let brightness_label = field_label(i18n, "brightness-title");
    controls.append(&brightness_label);
    let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        brightness_label.upcast_ref()
    ])]);
    brightness.set_draw_value(true);
    brightness.set_value_pos(gtk::PositionType::Right);
    brightness.set_hexpand(true);
    controls.append(&brightness);
    let intensity_label = field_label(i18n, "intensity-title");
    controls.append(&intensity_label);
    let intensity_strings = [
        i18n.text("intensity-subtle"),
        i18n.text("intensity-moderate"),
        i18n.text("intensity-high"),
        i18n.text("intensity-extreme"),
    ];
    let intensity_refs: Vec<&str> = intensity_strings.iter().map(String::as_str).collect();
    let intensity = gtk::DropDown::from_strings(&intensity_refs);
    intensity.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        intensity_label.upcast_ref()
    ])]);
    controls.append(&intensity);
    content.append(&controls);

    let action = gtk::Button::with_label(&i18n.text("action-start"));
    action.set_css_classes(&["suggested-action", "pill", "sync-action"]);
    action.set_halign(gtk::Align::Center);
    action.set_size_request(220, 58);
    content.append(&action);
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.set_justify(gtk::Justification::Center);
    error.set_css_classes(&["error", "caption"]);
    content.append(&error);
    let privacy = gtk::Label::new(Some(&i18n.text("privacy-note")));
    privacy.set_wrap(true);
    privacy.set_justify(gtk::Justification::Center);
    privacy.set_css_classes(&["dim-label", "caption"]);
    content.append(&privacy);

    let mode = [video, game, music, scene];
    for (index, button) in mode.iter().enumerate() {
        let app = app.clone();
        button.connect_toggled(move |button| {
            if !button.is_active() || app.0.applying.get() {
                return;
            }
            let mode = [
                SyncMode::Video,
                SyncMode::Game,
                SyncMode::Music,
                SyncMode::Scene,
            ][index];
            app.update_sync(move |settings| settings.mode = mode, false);
        });
    }
    {
        let app = app.clone();
        brightness.connect_value_changed(move |scale| {
            if app.0.applying.get() {
                return;
            }
            let value = scale.value().round().clamp(0.0, 100.0) as u8;
            if let Ok(value) = Brightness::new(value) {
                app.update_sync(move |settings| settings.brightness = value, true);
            }
        });
    }
    {
        let app = app.clone();
        intensity.connect_selected_notify(move |dropdown| {
            if app.0.applying.get() {
                return;
            }
            let intensity = [
                Intensity::Subtle,
                Intensity::Moderate,
                Intensity::High,
                Intensity::Extreme,
            ][dropdown.selected().min(3) as usize];
            app.update_sync(move |settings| settings.intensity = intensity, false);
        });
    }
    {
        let app = app.clone();
        action.connect_clicked(move |_| {
            let stop = dashboard_state(
                app.0.model.borrow().connected,
                app.0.model.borrow().status.as_ref(),
            )
            .action_is_stop;
            app.send(if stop { Request::Stop } else { Request::Start });
        });
    }

    DashboardWidgets {
        page: scroll_page(&content),
        bridge,
        area,
        error,
        action,
        mode,
        brightness,
        intensity,
        mode_row: modes,
    }
}

struct SetupWidgets {
    page: gtk::ScrolledWindow,
    bridges: gtk::ListBox,
    areas: gtk::ListBox,
    discover: gtk::Button,
    complete: gtk::Button,
    refresh: gtk::Button,
}

fn build_setup(app: &App, i18n: &I18n) -> SetupWidgets {
    let content = page_content(i18n, "setup-title", "setup-subtitle");
    let bridge_card = card();
    bridge_card.append(&section_label(i18n, "discover-title"));
    bridge_card.append(&description(i18n, "discover-description"));
    let discover = gtk::Button::with_label(&i18n.text("action-discover"));
    discover.set_halign(gtk::Align::Start);
    discover.set_css_classes(&["suggested-action"]);
    let app_clone = app.clone();
    discover.connect_clicked(move |_| {
        app_clone.send(Request::DiscoverBridges);
    });
    bridge_card.append(&discover);
    let bridges = list_box();
    bridge_card.append(&bridges);
    content.append(&bridge_card);

    let pair_card = card();
    pair_card.append(&section_label(i18n, "pair-title"));
    pair_card.append(&description(i18n, "pair-description"));
    let complete = gtk::Button::with_label(&i18n.text("action-complete-pairing"));
    complete.set_halign(gtk::Align::Start);
    let app_clone = app.clone();
    complete.connect_clicked(move |_| {
        let bridge_id = app_clone.0.model.borrow().pairing_bridge.clone();
        if let Some(bridge_id) = bridge_id {
            app_clone.send(Request::CompletePairing { bridge_id });
        }
    });
    pair_card.append(&complete);
    content.append(&pair_card);

    let areas_card = card();
    areas_card.append(&section_label(i18n, "areas-title"));
    areas_card.append(&description(i18n, "areas-description"));
    let refresh = gtk::Button::with_label(&i18n.text("action-refresh"));
    refresh.set_halign(gtk::Align::Start);
    let app_clone = app.clone();
    refresh.connect_clicked(move |_| {
        app_clone.send(Request::RefreshAreas);
    });
    areas_card.append(&refresh);
    let areas = list_box();
    areas_card.append(&areas);
    content.append(&areas_card);
    SetupWidgets {
        page: scroll_page(&content),
        bridges,
        areas,
        discover,
        complete,
        refresh,
    }
}

struct ProfileWidgets {
    page: gtk::ScrolledWindow,
    list: gtk::ListBox,
    create_row: gtk::Box,
    entry: gtk::Entry,
    create: gtk::Button,
}

fn build_profiles(app: &App, i18n: &I18n) -> ProfileWidgets {
    let content = page_content(i18n, "profiles-title", "profiles-subtitle");
    let create_card = card();
    let create_label = section_label(i18n, "profile-new-title");
    create_card.append(&create_label);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let entry = gtk::Entry::builder()
        .placeholder_text(i18n.text("profile-name"))
        .hexpand(true)
        .build();
    entry.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        create_label.upcast_ref()
    ])]);
    let create = gtk::Button::with_label(&i18n.text("action-create-profile"));
    create.set_css_classes(&["suggested-action"]);
    row.append(&entry);
    row.append(&create);
    create_card.append(&row);
    content.append(&create_card);
    let profile_card = card();
    let list = list_box();
    profile_card.append(&list);
    content.append(&profile_card);
    let app_clone = app.clone();
    let entry_for_create = entry.clone();
    create.connect_clicked(move |_| {
        let name = entry_for_create.text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let model = app_clone.0.model.borrow();
        let Some(config) = model.config.as_ref() else {
            return;
        };
        let profile = Profile {
            id: ProfileId::new(),
            name,
            settings: model
                .optimistic_sync
                .clone()
                .unwrap_or_else(|| config.sync.clone()),
            display: config.selected_display.clone(),
            area: config.selected_area.clone(),
        };
        drop(model);
        entry_for_create.set_text("");
        app_clone.send(Request::CreateProfile { profile });
    });
    {
        let app = app.clone();
        entry.connect_changed(move |_| app.render());
    }
    ProfileWidgets {
        page: scroll_page(&content),
        list,
        create_row: row,
        entry,
        create,
    }
}

struct SettingsWidgets {
    page: gtk::ScrolledWindow,
    capture_backend: gtk::DropDown,
    restore: gtk::Switch,
    auto_start: gtk::Switch,
    language: gtk::DropDown,
    capture_backends: Vec<CaptureBackend>,
}

fn build_settings(app: &App, i18n: &I18n) -> SettingsWidgets {
    let content = page_content(i18n, "settings-title", "settings-subtitle");
    let capture_card = card();
    let capture_label = section_label(i18n, "capture-title");
    capture_card.append(&capture_label);
    let capture_backends = app
        .0
        .model
        .borrow()
        .capabilities
        .as_ref()
        .map_or_else(Vec::new, |capabilities| {
            capabilities.capture_backends.clone()
        });
    let capture_values: Vec<String> = capture_backends
        .iter()
        .map(|backend| {
            i18n.text(match backend {
                CaptureBackend::PortalPipewire => "capture-portal",
                CaptureBackend::Grim => "capture-grim",
            })
        })
        .collect();
    let refs: Vec<&str> = capture_values.iter().map(String::as_str).collect();
    let capture_backend = gtk::DropDown::from_strings(&refs);
    capture_backend.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        capture_label.upcast_ref()
    ])]);
    capture_card.append(&capture_backend);
    if capture_backends.is_empty() {
        capture_card.append(&description(i18n, "capture-unavailable"));
    }
    capture_card.append(&description(i18n, "capture-portal-description"));
    capture_card.append(&description(i18n, "capture-grim-description"));
    content.append(&capture_card);

    let behavior = card();
    behavior.append(&section_label(i18n, "behavior-title"));
    let restore = switch_row(&behavior, i18n, "restore-title", "restore-description");
    let auto_start = switch_row(&behavior, i18n, "autostart-title", "autostart-description");
    content.append(&behavior);

    let language_card = card();
    let language_label = section_label(i18n, "language-title");
    language_card.append(&language_label);
    let language_values = [
        i18n.text("language-system"),
        i18n.text("language-english"),
        i18n.text("language-german"),
    ];
    let refs: Vec<&str> = language_values.iter().map(String::as_str).collect();
    let language = gtk::DropDown::from_strings(&refs);
    language.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        language_label.upcast_ref()
    ])]);
    language_card.append(&language);
    content.append(&language_card);

    let about_card = card();
    about_card.append(&section_label(i18n, "about-title"));
    let about = gtk::Button::with_label(&i18n.text("action-about"));
    about.set_halign(gtk::Align::Start);
    let window = app.0.window.clone();
    let about_name = i18n.text("app-name");
    let about_comments = i18n.text("about-comments");
    let about_developer = i18n.text("about-developer");
    about.connect_clicked(move |_| {
        let dialog = adw::AboutDialog::builder()
            .application_name(&about_name)
            .application_icon(APPLICATION_ID)
            .developer_name(&about_developer)
            .version(env!("CARGO_PKG_VERSION"))
            .comments(&about_comments)
            .website("https://github.com/mahype/omarchy-lightsync")
            .issue_url("https://github.com/mahype/omarchy-lightsync/issues")
            .license_type(gtk::License::MitX11)
            .build();
        dialog.present(Some(&window));
    });
    about_card.append(&about);
    content.append(&about_card);

    {
        let app = app.clone();
        let capture_backends = capture_backends.clone();
        capture_backend.connect_selected_notify(move |dropdown| {
            let Some(backend) = capture_backends.get(dropdown.selected() as usize).copied() else {
                return;
            };
            app.update_config(ConfigUpdate {
                capture_backend: Some(backend),
                ..ConfigUpdate::default()
            });
        });
    }
    connect_switch(app, &restore, |value| ConfigUpdate {
        restore_on_stop: Some(value),
        ..ConfigUpdate::default()
    });
    connect_switch(app, &auto_start, |value| ConfigUpdate {
        auto_start_sync: Some(value),
        ..ConfigUpdate::default()
    });
    {
        let app = app.clone();
        language.connect_selected_notify(move |dropdown| {
            let language = match dropdown.selected() {
                1 => Language::En,
                2 => Language::De,
                _ => Language::System,
            };
            app.update_config(ConfigUpdate {
                language: Some(language),
                ..ConfigUpdate::default()
            });
        });
    }
    SettingsWidgets {
        page: scroll_page(&content),
        capture_backend,
        restore,
        auto_start,
        language,
        capture_backends,
    }
}

struct DiagnosticWidgets {
    page: gtk::ScrolledWindow,
    service: gtk::Label,
    bridge: gtk::Label,
    capture: gtk::Label,
    sync: gtk::Label,
    fps: gtk::Label,
}

fn build_diagnostics(i18n: &I18n) -> DiagnosticWidgets {
    let content = page_content(i18n, "diagnostics-title", "diagnostics-subtitle");
    let diagnostics = card();
    let service = value_row(&diagnostics, i18n, "service-title");
    let bridge = value_row(&diagnostics, i18n, "bridge-title");
    let capture = value_row(&diagnostics, i18n, "capture-state-title");
    let sync = value_row(&diagnostics, i18n, "sync-state-title");
    let fps = value_row(&diagnostics, i18n, "fps-title");
    content.append(&diagnostics);
    DiagnosticWidgets {
        page: scroll_page(&content),
        service,
        bridge,
        capture,
        sync,
        fps,
    }
}

fn render_bridges(app: &App, i18n: &I18n, ui: &Ui, model: &Model) {
    clear_list(&ui.bridges);
    ui.bridge_actions.borrow_mut().clear();
    if model.bridges.is_empty() {
        ui.bridges.append(&plain_row(&i18n.text("discover-empty")));
    }
    for bridge in &model.bridges {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(10);
        row.set_margin_end(10);
        let text = bridge
            .name
            .clone()
            .unwrap_or_else(|| i18n.text("bridge-unnamed"));
        let label = gtk::Label::new(Some(&format!("{text} - {}", bridge.host)));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let pair = gtk::Button::with_label(&i18n.text("action-pair"));
        let candidate = bridge.clone();
        let app = app.clone();
        pair.update_property(&[gtk::accessible::Property::Label(&i18n.text_with_arg(
            "action-pair-context",
            "name",
            &text,
        ))]);
        pair.set_sensitive(
            model.connected
                && !model.pending.contains_key(&Operation::BeginPairing)
                && model
                    .capabilities
                    .as_ref()
                    .is_some_and(|capabilities| capabilities.discovery),
        );
        pair.connect_clicked(move |button| {
            button.set_sensitive(false);
            app.send(Request::BeginPairing {
                bridge: candidate.clone(),
            });
        });
        row.append(&label);
        row.append(&pair);
        ui.bridge_actions.borrow_mut().push(pair);
        ui.bridges.append(&row);
    }
}

fn render_areas(app: &App, i18n: &I18n, ui: &Ui, model: &Model) {
    clear_list(&ui.areas);
    ui.area_actions.borrow_mut().clear();
    if model.areas.is_empty() {
        ui.areas.append(&plain_row(&i18n.text("areas-empty")));
    }
    for area in &model.areas {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(10);
        row.set_margin_end(10);
        let label = gtk::Label::new(Some(&area.name));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        let selected = model
            .config
            .as_ref()
            .and_then(|config| config.selected_area.as_ref())
            == Some(&area.id);
        let button = gtk::Button::with_label(&i18n.text(if selected {
            "selected"
        } else {
            "action-select"
        }));
        button.set_sensitive(!selected && model.connected);
        button.update_property(&[gtk::accessible::Property::Label(&i18n.text_with_arg(
            "action-select-context",
            "name",
            &area.name,
        ))]);
        let area_id = area.id.clone();
        let app = app.clone();
        button.connect_clicked(move |_| {
            app.send(Request::SelectArea {
                area_id: Some(area_id.clone()),
            });
        });
        row.append(&label);
        row.append(&button);
        ui.area_actions.borrow_mut().push((button, selected));
        ui.areas.append(&row);
    }
}

fn render_profiles(app: &App, i18n: &I18n, ui: &Ui, model: &Model) {
    clear_list(&ui.profiles);
    ui.profile_actions.borrow_mut().clear();
    let Some(config) = model.config.as_ref() else {
        ui.profiles.append(&plain_row(&i18n.text("profiles-empty")));
        return;
    };
    if config.profiles.is_empty() {
        ui.profiles.append(&plain_row(&i18n.text("profiles-empty")));
    }
    for profile in &config.profiles {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(10);
        row.set_margin_end(10);
        let label = gtk::Label::new(Some(&profile.name));
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        let active = config.active_profile == Some(profile.id);
        let activate =
            gtk::Button::with_label(&i18n.text(if active { "active" } else { "action-activate" }));
        activate.set_sensitive(
            !active && model.connected && !model.pending.contains_key(&Operation::ActivateProfile),
        );
        activate.update_property(&[gtk::accessible::Property::Label(&i18n.text_with_arg(
            "action-activate-context",
            "name",
            &profile.name,
        ))]);
        let id = profile.id;
        let app_activate = app.clone();
        activate.connect_clicked(move |_| {
            app_activate.send(Request::ActivateProfile {
                profile_id: Some(id),
            });
        });
        let delete = gtk::Button::with_label(&i18n.text("action-delete"));
        delete.set_css_classes(&["destructive-action"]);
        delete.set_sensitive(
            model.connected && !model.pending.contains_key(&Operation::DeleteProfile),
        );
        delete.update_property(&[gtk::accessible::Property::Label(&i18n.text_with_arg(
            "action-delete-context",
            "name",
            &profile.name,
        ))]);
        let id = profile.id;
        let app_delete = app.clone();
        let profile_name = profile.name.clone();
        delete.connect_clicked(move |_| {
            let i18n = app_delete.i18n();
            let dialog = adw::AlertDialog::builder()
                .heading(i18n.text("delete-profile-title"))
                .body(i18n.text_with_arg("delete-profile-body", "name", &profile_name))
                .build();
            dialog.add_response("cancel", &i18n.text("action-cancel"));
            dialog.add_response("delete", &i18n.text("action-delete"));
            dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let app = app_delete.clone();
            dialog.connect_response(Some("delete"), move |_, _| {
                app.send(Request::DeleteProfile { profile_id: id });
            });
            dialog.present(Some(&app_delete.0.window));
        });
        row.append(&label);
        row.append(&activate);
        row.append(&delete);
        ui.profile_actions
            .borrow_mut()
            .push((activate, delete, active));
        ui.profiles.append(&row);
    }
}

fn page_content(i18n: &I18n, title_id: &str, subtitle_id: &str) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(28);
    content.set_margin_start(24);
    content.set_margin_end(24);
    let title = gtk::Label::new(Some(&i18n.text(title_id)));
    title.set_css_classes(&["title-1"]);
    title.set_halign(gtk::Align::Start);
    let subtitle = gtk::Label::new(Some(&i18n.text(subtitle_id)));
    subtitle.set_wrap(true);
    subtitle.set_halign(gtk::Align::Start);
    subtitle.set_css_classes(&["dim-label"]);
    content.append(&title);
    content.append(&subtitle);
    content
}

fn scroll_page(content: &gtk::Box) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(content)
        .build()
}

fn card() -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.set_css_classes(&["card"]);
    card.set_margin_top(2);
    card.set_margin_bottom(2);
    card.set_margin_start(2);
    card.set_margin_end(2);
    card
}

fn section_label(i18n: &I18n, id: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(&i18n.text(id)));
    label.set_css_classes(&["heading"]);
    label.set_halign(gtk::Align::Start);
    label
}

fn field_label(i18n: &I18n, id: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(&i18n.text(id)));
    label.set_css_classes(&["caption", "dim-label"]);
    label.set_halign(gtk::Align::Start);
    label.set_margin_top(4);
    label
}

fn description(i18n: &I18n, id: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(&i18n.text(id)));
    label.set_wrap(true);
    label.set_halign(gtk::Align::Start);
    label.set_css_classes(&["dim-label"]);
    label
}

fn value_row(parent: &gtk::Box, i18n: &I18n, id: &str) -> gtk::Label {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let title = gtk::Label::new(Some(&i18n.text(id)));
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Start);
    let value = gtk::Label::new(None);
    value.update_relation(&[gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()])]);
    value.set_css_classes(&["dim-label"]);
    value.set_halign(gtk::Align::End);
    row.append(&title);
    row.append(&value);
    parent.append(&row);
    value
}

fn switch_row(parent: &gtk::Box, i18n: &I18n, title_id: &str, description_id: &str) -> gtk::Switch {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    let title = gtk::Label::new(Some(&i18n.text(title_id)));
    title.set_halign(gtk::Align::Start);
    let description = description(i18n, description_id);
    description.set_css_classes(&["dim-label", "caption"]);
    labels.append(&title);
    labels.append(&description);
    let control = gtk::Switch::new();
    control.set_valign(gtk::Align::Center);
    control.update_relation(&[gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()])]);
    row.append(&labels);
    row.append(&control);
    parent.append(&row);
    control
}

fn connect_switch(
    app: &App,
    control: &gtk::Switch,
    update: impl Fn(bool) -> ConfigUpdate + 'static,
) {
    let app = app.clone();
    control.connect_active_notify(move |control| app.update_config(update(control.is_active())));
}

fn list_box() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_css_classes(&["boxed-list"]);
    list
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn plain_row(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_margin_top(12);
    label.set_margin_bottom(12);
    label.set_margin_start(10);
    label.set_margin_end(10);
    label.set_css_classes(&["dim-label"]);
    label
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".atmospheric-header { background: linear-gradient(110deg, alpha(@accent_bg_color, .28), alpha(#6b4cff, .16), transparent); border-bottom: 1px solid alpha(@accent_color, .2); }\n\
         .sync-mark { font-size: 30px; color: @accent_color; }\n\
         .status-chip { padding: 6px 12px; border-radius: 999px; background: alpha(@accent_bg_color, .16); }\n\
         .card { padding: 18px; border-radius: 14px; background: alpha(@card_bg_color, .92); box-shadow: 0 1px 3px alpha(black, .16); }\n\
         .sync-action { font-size: 17px; font-weight: 700; margin-top: 4px; }\n\
         .mode-button { padding: 9px 6px; }\n\
         .navigation-sidebar { border-right: 1px solid alpha(@borders, .55); }",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("GTK display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn main() -> glib::ExitCode {
    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    application.connect_startup(|_| install_css());
    application.connect_activate(|application| {
        if let Some(window) = application.active_window() {
            window.present();
            return;
        }
        let (event_sender, event_receiver) = mpsc::channel();
        let worker = Worker::start(event_sender);
        let app = App::new(application, worker);
        glib::timeout_add_local(Duration::from_millis(60), move || {
            while let Ok(event) = event_receiver.try_recv() {
                app.handle(event);
            }
            glib::ControlFlow::Continue
        });
    });
    application.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_updates_have_an_independent_pending_operation() {
        assert_eq!(
            operation(&Request::UpdateConfig {
                update: ConfigUpdate {
                    sync: Some(SyncSettings::default()),
                    ..ConfigUpdate::default()
                },
            }),
            Some(Operation::UpdateSync)
        );
        assert_eq!(
            operation(&Request::UpdateConfig {
                update: ConfigUpdate {
                    restore_on_stop: Some(false),
                    ..ConfigUpdate::default()
                },
            }),
            Some(Operation::UpdateConfig)
        );
        assert_ne!(operation(&Request::Start), operation(&Request::Stop));
    }

    #[test]
    fn areas_are_not_fetched_for_clean_unconfigured_startup() {
        assert!(!should_refresh_areas(&StatusSnapshot::default()));
        let connected = StatusSnapshot {
            bridge: BridgeState::Connected,
            ..StatusSnapshot::default()
        };
        assert!(should_refresh_areas(&connected));
    }
}
