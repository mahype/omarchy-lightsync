use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use lightsync_domain::{
    AppConfig, AreaId, BridgeId, Brightness, CaptureBackend, CaptureState, ConfigUpdate,
    DiscoveredBridge, ErrorCode, Intensity, Language, PROTOCOL_VERSION, Profile, ProfileId,
    Request, ResponsePayload, ServiceState, StatusSnapshot, SyncMode, SyncState,
};
use lightsync_ipc::{Client, WatchOptions};
use serde_json::{Value, json};

const EXIT_FAILURE: u8 = 1;

#[derive(Debug, Parser)]
#[command(name = "lightsync", version, about = "Control the LightSync service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show current service status.
    Status(StatusArgs),
    /// Show service capabilities.
    Capabilities,
    /// Read or update configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Discover and configure a Hue bridge.
    Bridge {
        #[command(subcommand)]
        command: BridgeCommand,
    },
    /// List or select an entertainment area.
    Area {
        #[command(subcommand)]
        command: AreaCommand,
    },
    /// Manage synchronization profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Control synchronization.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Emit compact, one-record-per-line JSON.
    #[arg(long)]
    json: bool,
    /// Continue streaming status changes and reconnect on interruption.
    #[arg(long)]
    watch: bool,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show the current configuration.
    Show,
    /// Set one configuration value.
    Set {
        #[arg(value_enum)]
        key: ConfigKey,
        value: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConfigKey {
    Language,
    Backend,
    Mode,
    Intensity,
    Brightness,
    AudioReactive,
    RestoreOnStop,
    AutoStart,
}

#[derive(Debug, Subcommand)]
enum BridgeCommand {
    /// Discover bridges on the local network.
    Discover,
    /// Select a discovered bridge and begin pairing.
    Use {
        id: String,
        host: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Complete pairing after pressing the bridge link button.
    Pair {
        /// Bridge ID shown by `bridge use`.
        bridge_id: String,
    },
    /// Remove the configured bridge and area selection.
    Forget,
}

#[derive(Debug, Subcommand)]
enum AreaCommand {
    /// Refresh and list entertainment areas.
    List,
    /// Select an entertainment area by ID, or `none` to clear it.
    Select { area_id: String },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// List configured profiles.
    List,
    /// Create a profile from the current synchronization settings.
    Create { name: String },
    /// Delete a profile by UUID.
    Delete { profile_id: String },
    /// Activate a profile by UUID, or `none` to deactivate it.
    Activate { profile_id: String },
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    Start,
    Stop,
    Toggle,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lightsync: {error:#}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Command::Status(StatusArgs { json, watch: true }) = cli.command {
        return watch_status(json).await;
    }

    let client = Client::connect()
        .await
        .context("cannot connect to lightsyncd; is the user service running?")?;

    match cli.command {
        Command::Status(args) => show_status(&client, args.json).await,
        Command::Capabilities => show_capabilities(&client).await,
        Command::Config { command } => config_command(&client, command).await,
        Command::Bridge { command } => bridge_command(&client, command).await,
        Command::Area { command } => area_command(&client, command).await,
        Command::Profile { command } => profile_command(&client, command).await,
        Command::Sync { command } => sync_command(&client, command).await,
    }
}

async fn request(client: &Client, request: Request) -> Result<ResponsePayload> {
    match client.request(request).await {
        Ok(payload) => Ok(payload),
        Err(lightsync_ipc::Error::Remote(error)) => {
            if let Some(action) = error.action {
                bail!("{}; {action}", error.message)
            }
            bail!(error.message)
        }
        Err(error) => Err(error).context("service request failed"),
    }
}

async fn get_config(client: &Client) -> Result<AppConfig> {
    match request(client, Request::GetConfig).await? {
        ResponsePayload::Config(config) => Ok(config),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

async fn get_status(client: &Client) -> Result<StatusSnapshot> {
    match request(client, Request::GetStatus).await? {
        ResponsePayload::Status(status) => Ok(status),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

fn expect_acknowledged(payload: ResponsePayload) -> Result<()> {
    match payload {
        ResponsePayload::Acknowledged => Ok(()),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

fn expect_config(payload: ResponsePayload) -> Result<AppConfig> {
    match payload {
        ResponsePayload::Config(config) => Ok(config),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

fn expect_profile(payload: ResponsePayload) -> Result<Profile> {
    match payload {
        ResponsePayload::Profile(profile) => Ok(profile),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

async fn show_status(client: &Client, json_output: bool) -> Result<()> {
    let status = get_status(client).await?;
    let config = get_config(client).await.ok();
    if json_output {
        write_json_line(&status_document(&status, config.as_ref()))
    } else {
        println!("{}", human_status(&status, config.as_ref()));
        Ok(())
    }
}

async fn watch_status(json_output: bool) -> Result<()> {
    let client = Client::from_runtime().context("cannot locate the lightsyncd socket")?;
    let mut config = None;
    let mut subscription = client.watch_status(WatchOptions::default());
    while let Some(item) = subscription.recv().await {
        match item {
            Ok(status) => {
                // The status-only subscription does not carry ConfigChanged events. A
                // separate request keeps context current without affecting reconnects.
                match get_config(&client).await {
                    Ok(current) => config = Some(current),
                    Err(error) if config.is_none() => {
                        eprintln!("lightsync: configuration unavailable: {error:#}");
                    }
                    Err(_) => {}
                }
                if json_output {
                    write_json_line(&status_document(&status, config.as_ref()))?;
                } else {
                    println!("{}", human_status(&status, config.as_ref()));
                    io::stdout().flush().context("cannot flush status output")?;
                }
            }
            Err(error) => eprintln!("lightsync: status stream interrupted; reconnecting: {error}"),
        }
    }
    bail!("status watcher stopped unexpectedly")
}

fn write_json_line(value: &Value) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).context("cannot encode status JSON")?;
    output
        .write_all(b"\n")
        .context("cannot write status JSON")?;
    output.flush().context("cannot flush status JSON")
}

fn status_document(status: &StatusSnapshot, config: Option<&AppConfig>) -> Value {
    let detail = status.error.as_ref().map(|error| error.message.as_str());
    let mut document = json!({
        "protocol_version": PROTOCOL_VERSION,
        "service": component(service_state(status.service), detail_for(status, Component::Service, detail)),
        "bridge": component(bridge_state(status.bridge), detail_for(status, Component::Bridge, detail)),
        "capture": component(capture_state(status.capture), detail_for(status, Component::Capture, detail)),
        "sync": component(sync_state(status.sync), detail_for(status, Component::Sync, detail)),
    });

    if let Some(area_id) = status
        .active_area
        .as_ref()
        .or_else(|| config.and_then(|config| config.selected_area.as_ref()))
    {
        document["area"] = json!({ "id": area_id.as_str(), "name": area_id.as_str() });
    }
    if let Some(profile) = config.and_then(active_profile) {
        document["profile"] = json!({ "id": profile.id.to_string(), "name": profile.name });
    }
    if let Some(frames_per_second) = status.frames_per_second {
        document["details"] = json!({ "frames_per_second": frames_per_second });
    }
    document
}

#[derive(Clone, Copy)]
enum Component {
    Service,
    Bridge,
    Capture,
    Sync,
}

fn detail_for<'a>(
    status: &StatusSnapshot,
    component: Component,
    detail: Option<&'a str>,
) -> Option<&'a str> {
    let code = status.error.as_ref().map(|error| error.code);
    let target = match code {
        Some(
            ErrorCode::BridgeNotFound
            | ErrorCode::BridgeUnavailable
            | ErrorCode::LinkButtonRequired,
        ) => Component::Bridge,
        Some(
            ErrorCode::CapturePermissionDenied
            | ErrorCode::CaptureUnavailable
            | ErrorCode::DisplayNotFound,
        ) => Component::Capture,
        Some(ErrorCode::SyncUnavailable | ErrorCode::Conflict | ErrorCode::AreaNotFound) => {
            Component::Sync
        }
        _ => Component::Service,
    };
    (std::mem::discriminant(&component) == std::mem::discriminant(&target))
        .then_some(detail)
        .flatten()
}

fn component(state: &'static str, detail: Option<&str>) -> Value {
    match detail {
        Some(detail) => json!({ "state": state, "detail": detail }),
        None => json!({ "state": state }),
    }
}

const fn service_state(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Starting => "starting",
        ServiceState::Ready => "ready",
        ServiceState::Stopping => "stopping",
        ServiceState::Failed => "failed",
    }
}

const fn bridge_state(state: lightsync_domain::BridgeState) -> &'static str {
    use lightsync_domain::BridgeState;
    match state {
        BridgeState::Unconfigured => "needs_setup",
        BridgeState::Disconnected => "unreachable",
        BridgeState::Discovering => "discovering",
        BridgeState::Pairing => "pairing",
        BridgeState::Connecting => "connecting",
        BridgeState::Connected => "ready",
    }
}

const fn capture_state(state: CaptureState) -> &'static str {
    match state {
        CaptureState::Idle => "idle",
        CaptureState::RequestingPermission => "requesting_permission",
        CaptureState::Capturing => "active",
        CaptureState::Failed => "failed",
    }
}

const fn sync_state(state: SyncState) -> &'static str {
    match state {
        SyncState::Stopped => "idle",
        SyncState::Starting => "starting",
        SyncState::Running => "active",
        SyncState::Stopping => "stopping",
        SyncState::Failed => "failed",
    }
}

fn active_profile(config: &AppConfig) -> Option<&Profile> {
    let active = config.active_profile?;
    config.profiles.iter().find(|profile| profile.id == active)
}

fn human_status(status: &StatusSnapshot, config: Option<&AppConfig>) -> String {
    let area = status
        .active_area
        .as_ref()
        .or_else(|| config.and_then(|config| config.selected_area.as_ref()))
        .map(|id| format!(", area {id}"))
        .unwrap_or_default();
    let profile = config
        .and_then(active_profile)
        .map(|profile| format!(", profile {}", profile.name))
        .unwrap_or_default();
    let detail = status
        .error
        .as_ref()
        .map(|error| format!(": {}", error.message))
        .unwrap_or_default();
    format!(
        "Service {}, bridge {}, capture {}, sync {}{area}{profile}{detail}",
        service_state(status.service),
        bridge_state(status.bridge),
        capture_state(status.capture),
        sync_state(status.sync),
    )
}

async fn show_capabilities(client: &Client) -> Result<()> {
    let capabilities = match request(client, Request::GetCapabilities).await? {
        ResponsePayload::Capabilities(capabilities) => capabilities,
        other => bail!("service returned an unexpected response: {other:?}"),
    };
    println!(
        "Protocol: {}\nCapture backends: {}\nSync modes: {}\nAudio reactive: {}\nDiscovery: {}",
        capabilities
            .protocol_versions
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        capabilities
            .capture_backends
            .iter()
            .map(|value| capture_backend_name(*value))
            .collect::<Vec<_>>()
            .join(", "),
        capabilities
            .sync_modes
            .iter()
            .map(|value| sync_mode_name(*value))
            .collect::<Vec<_>>()
            .join(", "),
        yes_no(capabilities.audio_reactive),
        yes_no(capabilities.discovery),
    );
    Ok(())
}

async fn config_command(client: &Client, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Show => print_config(&get_config(client).await?),
        ConfigCommand::Set { key, value } => {
            let config = get_config(client).await?;
            let update = config_update(&config, key, &value)?;
            let _config = expect_config(request(client, Request::UpdateConfig { update }).await?)?;
            println!("Configuration updated.");
            Ok(())
        }
    }
}

fn print_config(config: &AppConfig) -> Result<()> {
    println!("language={}", language_name(config.language));
    println!("backend={}", capture_backend_name(config.capture_backend));
    println!("mode={}", sync_mode_name(config.sync.mode));
    println!("intensity={}", intensity_name(config.sync.intensity));
    println!("brightness={}", config.sync.brightness.get());
    println!("audio-reactive={}", config.sync.audio_reactive);
    println!("restore-on-stop={}", config.restore_on_stop);
    println!("launch-at-login={}", config.launch_at_login);
    println!("auto-start={}", config.auto_start_sync);
    Ok(())
}

fn config_update(config: &AppConfig, key: ConfigKey, value: &str) -> Result<ConfigUpdate> {
    let mut update = ConfigUpdate::default();
    match key {
        ConfigKey::Language => update.language = Some(parse_language(value)?),
        ConfigKey::Backend => update.capture_backend = Some(parse_backend(value)?),
        ConfigKey::Mode => {
            let mut sync = config.sync.clone();
            sync.mode = parse_mode(value)?;
            update.sync = Some(sync);
        }
        ConfigKey::Intensity => {
            let mut sync = config.sync.clone();
            sync.intensity = parse_intensity(value)?;
            update.sync = Some(sync);
        }
        ConfigKey::Brightness => {
            let mut sync = config.sync.clone();
            let brightness = value
                .parse::<u8>()
                .context("brightness must be an integer from 0 to 100")?;
            sync.brightness = Brightness::new(brightness).context("invalid brightness")?;
            update.sync = Some(sync);
        }
        ConfigKey::AudioReactive => {
            let mut sync = config.sync.clone();
            sync.audio_reactive = parse_bool(value)?;
            update.sync = Some(sync);
        }
        ConfigKey::RestoreOnStop => update.restore_on_stop = Some(parse_bool(value)?),
        ConfigKey::AutoStart => update.auto_start_sync = Some(parse_bool(value)?),
    }
    Ok(update)
}

async fn bridge_command(client: &Client, command: BridgeCommand) -> Result<()> {
    match command {
        BridgeCommand::Discover => match request(client, Request::DiscoverBridges).await? {
            ResponsePayload::Bridges(bridges) => {
                if bridges.is_empty() {
                    println!("No bridges found.");
                }
                for bridge in bridges {
                    println!(
                        "{}\t{}\t{}",
                        bridge.id,
                        bridge.host,
                        bridge.name.as_deref().unwrap_or("")
                    );
                }
                Ok(())
            }
            other => bail!("service returned an unexpected response: {other:?}"),
        },
        BridgeCommand::Use { id, host, name } => {
            if host.trim().is_empty() {
                bail!("bridge host must not be empty");
            }
            let bridge = DiscoveredBridge {
                id: BridgeId::new(id)?,
                host,
                name,
            };
            let bridge_id = bridge.id.clone();
            expect_acknowledged(request(client, Request::BeginPairing { bridge }).await?)?;
            print_pairing_result(client, &bridge_id, false).await;
            Ok(())
        }
        BridgeCommand::Pair { bridge_id } => {
            let bridge_id = BridgeId::new(bridge_id)?;
            expect_acknowledged(
                request(
                    client,
                    Request::CompletePairing {
                        bridge_id: bridge_id.clone(),
                    },
                )
                .await?,
            )?;
            print_pairing_result(client, &bridge_id, true).await;
            Ok(())
        }
        BridgeCommand::Forget => {
            let _config = expect_config(request(client, Request::ForgetBridge).await?)?;
            println!("Bridge forgotten.");
            Ok(())
        }
    }
}

async fn print_pairing_result(client: &Client, bridge_id: &BridgeId, continuation: bool) {
    if matches!(
        get_status(client).await,
        Ok(StatusSnapshot {
            bridge: lightsync_domain::BridgeState::Connected,
            ..
        })
    ) {
        println!("Bridge paired.");
    } else if continuation {
        println!(
            "Still waiting for the bridge link button. Press it, then run `lightsync bridge pair {bridge_id}` again."
        );
    } else {
        println!(
            "Waiting for the bridge link button. Press it, then run `lightsync bridge pair {bridge_id}`."
        );
    }
}

async fn area_command(client: &Client, command: AreaCommand) -> Result<()> {
    match command {
        AreaCommand::List => match request(client, Request::RefreshAreas).await? {
            ResponsePayload::Areas(areas) => {
                let selected = get_config(client)
                    .await
                    .ok()
                    .and_then(|config| config.selected_area);
                if areas.is_empty() {
                    println!("No entertainment areas found.");
                }
                for area in areas {
                    let marker = if selected.as_ref() == Some(&area.id) {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "{marker} {}\t{}\t{} lights",
                        area.id,
                        area.name,
                        area.channels.len()
                    );
                }
                Ok(())
            }
            other => bail!("service returned an unexpected response: {other:?}"),
        },
        AreaCommand::Select { area_id } => {
            let area_id = if area_id.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(AreaId::new(area_id)?)
            };
            let _config = expect_config(request(client, Request::SelectArea { area_id }).await?)?;
            println!("Area selection updated.");
            Ok(())
        }
    }
}

async fn profile_command(client: &Client, command: ProfileCommand) -> Result<()> {
    match command {
        ProfileCommand::List => {
            let config = get_config(client).await?;
            if config.profiles.is_empty() {
                println!("No profiles configured.");
            }
            for profile in config.profiles {
                let marker = if config.active_profile == Some(profile.id) {
                    "*"
                } else {
                    " "
                };
                println!("{marker} {}\t{}", profile.id, profile.name);
            }
            Ok(())
        }
        ProfileCommand::Create { name } => {
            if name.trim().is_empty() {
                bail!("profile name must not be empty");
            }
            let config = get_config(client).await?;
            let profile = Profile {
                id: ProfileId::new(),
                name,
                settings: config.sync,
                display: config.selected_display,
                area: config.selected_area,
            };
            let profile =
                expect_profile(request(client, Request::CreateProfile { profile }).await?)?;
            println!("Created profile {} ({})", profile.name, profile.id);
            Ok(())
        }
        ProfileCommand::Delete { profile_id } => {
            let profile_id = parse_profile_id(&profile_id)?;
            expect_acknowledged(request(client, Request::DeleteProfile { profile_id }).await?)?;
            println!("Profile deleted.");
            Ok(())
        }
        ProfileCommand::Activate { profile_id } => {
            let profile_id = if profile_id.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(parse_profile_id(&profile_id)?)
            };
            let _config =
                expect_config(request(client, Request::ActivateProfile { profile_id }).await?)?;
            println!("Active profile updated.");
            Ok(())
        }
    }
}

async fn sync_command(client: &Client, command: SyncCommand) -> Result<()> {
    let (request_value, message) = match command {
        SyncCommand::Start => (Request::Start, "Synchronization started."),
        SyncCommand::Stop => (Request::Stop, "Synchronization stopped."),
        SyncCommand::Toggle => (Request::Toggle, "Synchronization toggled."),
    };
    expect_acknowledged(request(client, request_value).await?)?;
    println!("{message}");
    Ok(())
}

fn parse_profile_id(value: &str) -> Result<ProfileId> {
    serde_json::from_value(Value::String(value.to_owned())).context("profile ID must be a UUID")
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => bail!("expected true or false, got `{value}`"),
    }
}

fn parse_language(value: &str) -> Result<Language> {
    match value {
        "system" => Ok(Language::System),
        "en" => Ok(Language::En),
        "de" => Ok(Language::De),
        _ => bail!("language must be one of: system, en, de"),
    }
}

fn parse_backend(value: &str) -> Result<CaptureBackend> {
    match value {
        "portal-pipewire" => Ok(CaptureBackend::PortalPipewire),
        "grim" => Ok(CaptureBackend::Grim),
        _ => bail!("backend must be one of: portal-pipewire, grim"),
    }
}

fn parse_mode(value: &str) -> Result<SyncMode> {
    match value {
        "video" => Ok(SyncMode::Video),
        "game" => Ok(SyncMode::Game),
        "music" => Ok(SyncMode::Music),
        "scene" => Ok(SyncMode::Scene),
        _ => bail!("mode must be one of: video, game, music, scene"),
    }
}

fn parse_intensity(value: &str) -> Result<Intensity> {
    match value {
        "subtle" => Ok(Intensity::Subtle),
        "moderate" => Ok(Intensity::Moderate),
        "high" => Ok(Intensity::High),
        "extreme" => Ok(Intensity::Extreme),
        _ => bail!("intensity must be one of: subtle, moderate, high, extreme"),
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
const fn language_name(value: Language) -> &'static str {
    match value {
        Language::System => "system",
        Language::En => "en",
        Language::De => "de",
    }
}
const fn capture_backend_name(value: CaptureBackend) -> &'static str {
    match value {
        CaptureBackend::PortalPipewire => "portal-pipewire",
        CaptureBackend::Grim => "grim",
    }
}
const fn sync_mode_name(value: SyncMode) -> &'static str {
    match value {
        SyncMode::Video => "video",
        SyncMode::Game => "game",
        SyncMode::Music => "music",
        SyncMode::Scene => "scene",
    }
}
const fn intensity_name(value: Intensity) -> &'static str {
    match value {
        Intensity::Subtle => "subtle",
        Intensity::Moderate => "moderate",
        Intensity::High => "high",
        Intensity::Extreme => "extreme",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightsync_domain::{ActionableError, BridgeState, ErrorCode};

    #[test]
    fn parser_accepts_status_watch_json() {
        let cli = Cli::try_parse_from(["lightsync", "status", "--watch", "--json"])
            .expect("parse status command");
        assert!(matches!(
            cli.command,
            Command::Status(StatusArgs {
                json: true,
                watch: true
            })
        ));
    }

    #[test]
    fn parser_covers_command_tree_and_rejects_unknown_config_keys() {
        for args in [
            vec!["lightsync", "capabilities"],
            vec!["lightsync", "config", "show"],
            vec!["lightsync", "config", "set", "brightness", "50"],
            vec!["lightsync", "bridge", "discover"],
            vec!["lightsync", "bridge", "use", "id", "192.0.2.1"],
            vec!["lightsync", "bridge", "pair", "001788fffe123456"],
            vec!["lightsync", "bridge", "forget"],
            vec!["lightsync", "area", "list"],
            vec!["lightsync", "area", "select", "area-id"],
            vec!["lightsync", "profile", "list"],
            vec!["lightsync", "profile", "create", "Movies"],
            vec!["lightsync", "sync", "toggle"],
        ] {
            Cli::try_parse_from(args).expect("command should parse");
        }
        assert!(Cli::try_parse_from(["lightsync", "config", "set", "secret", "value"]).is_err());
    }

    #[test]
    fn domain_states_map_to_model_vocabulary() {
        assert_eq!(bridge_state(BridgeState::Connected), "ready");
        assert_eq!(bridge_state(BridgeState::Unconfigured), "needs_setup");
        assert_eq!(bridge_state(BridgeState::Disconnected), "unreachable");
        assert_eq!(capture_state(CaptureState::Capturing), "active");
        assert_eq!(sync_state(SyncState::Stopped), "idle");
        assert_eq!(sync_state(SyncState::Running), "active");
    }

    #[test]
    fn status_json_matches_model_contract_and_omits_absent_context() {
        let value = status_document(&StatusSnapshot::default(), None);
        assert_eq!(value["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(value["service"]["state"], "ready");
        assert_eq!(value["bridge"]["state"], "needs_setup");
        assert_eq!(value["capture"]["state"], "idle");
        assert_eq!(value["sync"]["state"], "idle");
        assert!(value.get("area").is_none());
        assert!(value.get("profile").is_none());
        let encoded = serde_json::to_string(&value).expect("encode JSON");
        assert!(!encoded.contains('\n'));
    }

    #[test]
    fn status_json_includes_safe_area_profile_and_error_detail() {
        let area = AreaId::new("area-1").expect("area ID");
        let profile = Profile {
            id: ProfileId::new(),
            name: "Movies".into(),
            settings: Default::default(),
            display: None,
            area: Some(area.clone()),
        };
        let config = AppConfig {
            selected_area: Some(area.clone()),
            active_profile: Some(profile.id),
            profiles: vec![profile],
            ..AppConfig::default()
        };
        let mut error = ActionableError::new(ErrorCode::SyncUnavailable, "stream unavailable");
        error.details.insert("username".into(), "hue-secret".into());
        error
            .details
            .insert("client_key".into(), "dtls-secret".into());
        let status = StatusSnapshot {
            bridge: BridgeState::Connected,
            sync: SyncState::Failed,
            active_area: Some(area),
            error: Some(error),
            ..StatusSnapshot::default()
        };
        let value = status_document(&status, Some(&config));
        assert_eq!(value["area"]["name"], "area-1");
        assert_eq!(value["profile"]["name"], "Movies");
        assert_eq!(value["sync"]["detail"], "stream unavailable");
        assert!(value.get("error").is_none());
        let encoded = serde_json::to_string(&value).expect("encode JSON");
        assert!(!encoded.contains("hue-secret"));
        assert!(!encoded.contains("dtls-secret"));
    }

    #[test]
    fn status_json_exposes_configured_area_and_profile_while_idle() {
        let area = AreaId::new("desk").expect("area ID");
        let profile = Profile {
            id: ProfileId::new(),
            name: "Evening".into(),
            settings: Default::default(),
            display: None,
            area: Some(area.clone()),
        };
        let config = AppConfig {
            selected_area: Some(area),
            active_profile: Some(profile.id),
            profiles: vec![profile],
            ..AppConfig::default()
        };

        let value = status_document(&StatusSnapshot::default(), Some(&config));

        assert_eq!(value["area"]["id"], "desk");
        assert_eq!(value["profile"]["name"], "Evening");
    }

    #[test]
    fn mutation_response_types_match_the_daemon_contract() {
        let config = AppConfig::default();
        let profile = Profile {
            id: ProfileId::new(),
            name: "Movies".into(),
            settings: Default::default(),
            display: None,
            area: None,
        };

        // UpdateConfig, SelectArea, ActivateProfile, and ForgetBridge.
        for _ in 0..4 {
            assert_eq!(
                expect_config(ResponsePayload::Config(config.clone())).expect("config response"),
                config
            );
        }
        // CreateProfile and UpdateProfile.
        for _ in 0..2 {
            assert_eq!(
                expect_profile(ResponsePayload::Profile(profile.clone()))
                    .expect("profile response"),
                profile
            );
        }
        // BeginPairing, CompletePairing, DeleteProfile, Start, Stop, and Toggle.
        for _ in 0..6 {
            expect_acknowledged(ResponsePayload::Acknowledged).expect("acknowledged response");
        }
    }

    #[test]
    fn pairing_status_uses_the_public_model_vocabulary() {
        let status = StatusSnapshot {
            bridge: BridgeState::Pairing,
            ..StatusSnapshot::default()
        };
        let value = status_document(&status, None);
        assert_eq!(value["bridge"]["state"], "pairing");
        assert_eq!(value["sync"]["state"], "idle");
    }

    #[test]
    fn config_updates_preserve_other_sync_settings() {
        let config = AppConfig::default();
        let update = config_update(&config, ConfigKey::Brightness, "42").expect("valid update");
        let sync = update.sync.expect("sync update");
        assert_eq!(sync.brightness.get(), 42);
        assert_eq!(sync.mode, config.sync.mode);
        assert_eq!(sync.intensity, config.sync.intensity);
        assert!(config_update(&config, ConfigKey::Brightness, "101").is_err());
    }
}
