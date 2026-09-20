#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::{Certificate, Client, Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

const DISCOVERY_URL: &str = "https://discovery.meethue.com/";
const KEYRING_SERVICE: &str = "io.github.mahype.omarchy-lightsync";
const ENTERTAINMENT_PORT: u16 = 2100;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SAFE_ATTEMPTS: usize = 3;
const STREAM_STOP_TIMEOUT: Duration = Duration::from_secs(6);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeCandidate {
    pub id: String,
    pub address: IpAddr,
    pub name: String,
}

impl BridgeCandidate {
    /// Constructs a manually configured bridge. Its bridge ID remains the TLS
    /// server name, while `address` is used only for socket resolution.
    pub fn manual(id: impl Into<String>, address: IpAddr) -> Result<Self> {
        let id = normalize_bridge_id(&id.into())?;
        Ok(Self {
            id,
            address,
            name: "Hue Bridge".to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntertainmentArea {
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub active: bool,
    pub active_streamer: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelPosition {
    pub channel_id: u8,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelColor {
    pub channel_id: u8,
    pub red: u16,
    pub green: u16,
    pub blue: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LightSnapshot {
    lights: Vec<LightRestore>,
}

impl LightSnapshot {
    #[must_use]
    pub fn light_count(&self) -> usize {
        self.lights.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct LightRestore {
    id: String,
    state: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairAttempt {
    WaitingForLinkButton,
    Paired,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BridgeCredentials {
    application_key: String,
    client_key: String,
}

impl BridgeCredentials {
    pub fn new(application_key: String, client_key: String) -> Result<Self> {
        if application_key.is_empty() {
            bail!("Hue application key is empty");
        }
        let key = hex::decode(&client_key).context("Hue Entertainment key is not valid hex")?;
        if key.is_empty() {
            bail!("Hue Entertainment key is empty");
        }
        Ok(Self {
            application_key,
            client_key,
        })
    }

    #[must_use]
    pub fn application_key(&self) -> &str {
        &self.application_key
    }
}

impl fmt::Debug for BridgeCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BridgeCredentials")
            .field("application_key", &"[REDACTED]")
            .field("client_key", &"[REDACTED]")
            .finish()
    }
}

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self, bridge_id: &str) -> Result<BridgeCredentials>;
    async fn store(&self, bridge_id: &str, credentials: &BridgeCredentials) -> Result<()>;
    async fn forget(&self, bridge_id: &str) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct SecretServiceCredentialStore;

#[async_trait]
impl CredentialStore for SecretServiceCredentialStore {
    async fn load(&self, bridge_id: &str) -> Result<BridgeCredentials> {
        let bridge_id = normalize_bridge_id(bridge_id)?;
        tokio::task::spawn_blocking(move || {
            let value = keyring::Entry::new(KEYRING_SERVICE, &bridge_id)
                .context("failed to open Secret Service")?
                .get_password()
                .context("no Hue credentials found; pair the bridge first")?;
            let stored: BridgeCredentials =
                serde_json::from_str(&value).context("stored Hue credentials are invalid")?;
            BridgeCredentials::new(stored.application_key, stored.client_key)
                .context("stored Hue credentials are invalid")
        })
        .await
        .context("credential lookup task failed")?
    }

    async fn store(&self, bridge_id: &str, credentials: &BridgeCredentials) -> Result<()> {
        let bridge_id = normalize_bridge_id(bridge_id)?;
        let value = serde_json::to_string(credentials).context("failed to encode credentials")?;
        tokio::task::spawn_blocking(move || {
            keyring::Entry::new(KEYRING_SERVICE, &bridge_id)
                .context("failed to open Secret Service")?
                .set_password(&value)
                .context("failed to store Hue credentials in Secret Service")
        })
        .await
        .context("credential storage task failed")?
    }

    async fn forget(&self, bridge_id: &str) -> Result<()> {
        let bridge_id = normalize_bridge_id(bridge_id)?;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &bridge_id)
                .context("failed to open Secret Service")?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error).context("failed to forget Hue credentials"),
            }
        })
        .await
        .context("credential deletion task failed")?
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlMethod {
    Get,
    Post,
    Put,
}

#[derive(Clone)]
pub struct ControlRequest {
    pub method: ControlMethod,
    pub path: String,
    application_key: Option<String>,
    pub body: Option<Value>,
}

impl ControlRequest {
    #[must_use]
    pub fn application_key(&self) -> Option<&str> {
        self.application_key.as_deref()
    }
}

impl fmt::Debug for ControlRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field(
                "application_key",
                &self.application_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("body", &self.body)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct ControlResponse {
    pub status: u16,
    pub body: Value,
}

#[async_trait]
pub trait ControlTransport: Send + Sync {
    async fn execute(&self, request: ControlRequest) -> Result<ControlResponse>;
}

struct HttpsControlTransport {
    client: Client,
    host: String,
}

impl HttpsControlTransport {
    fn new(candidate: &BridgeCandidate) -> Result<Self> {
        let host = normalize_bridge_id(&candidate.id)?;
        // Signify publishes both roots; hostname and chain validation stay enabled.
        let legacy = Certificate::from_pem(include_bytes!("hue-root-bridge.pem"))
            .context("bundled legacy Hue root certificate is invalid")?;
        let current = Certificate::from_pem(include_bytes!("hue-root-ca-01.pem"))
            .context("bundled current Hue root certificate is invalid")?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .add_root_certificate(legacy)
            .add_root_certificate(current)
            .resolve(&host, SocketAddr::new(candidate.address, 443))
            .build()
            .context("failed to build certificate-validating Hue HTTPS client")?;
        Ok(Self { client, host })
    }
}

#[async_trait]
impl ControlTransport for HttpsControlTransport {
    async fn execute(&self, request: ControlRequest) -> Result<ControlResponse> {
        if !request.path.starts_with('/') || request.path.contains("..") {
            bail!("invalid Hue API path");
        }
        let method = match request.method {
            ControlMethod::Get => Method::GET,
            ControlMethod::Post => Method::POST,
            ControlMethod::Put => Method::PUT,
        };
        let mut builder = self
            .client
            .request(method, format!("https://{}{}", self.host, request.path));
        if let Some(key) = request.application_key {
            builder = builder.header("hue-application-key", key);
        }
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }
        let response = builder.send().await.context("Hue HTTPS request failed")?;
        let status = response.status().as_u16();
        let body = response
            .json()
            .await
            .context("Hue bridge returned malformed JSON")?;
        Ok(ControlResponse { status, body })
    }
}

pub trait StreamConnector: Send + Sync {
    fn connect(&self, address: IpAddr, identity: &str, psk: &[u8])
    -> Result<Box<dyn Write + Send>>;
}

#[derive(Debug, Default)]
pub struct OpenSslDtlsConnector;

impl StreamConnector for OpenSslDtlsConnector {
    fn connect(
        &self,
        address: IpAddr,
        identity: &str,
        psk: &[u8],
    ) -> Result<Box<dyn Write + Send>> {
        use openssl::ssl::{Ssl, SslContext, SslMethod, SslOptions, SslVersion};

        let identity = identity.as_bytes().to_vec();
        let psk = psk.to_vec();
        let mut context =
            SslContext::builder(SslMethod::dtls_client()).context("failed to initialize DTLS")?;
        context
            .set_min_proto_version(Some(SslVersion::DTLS1_2))
            .context("failed to require DTLS 1.2")?;
        context
            .set_max_proto_version(Some(SslVersion::DTLS1_2))
            .context("failed to limit DTLS to 1.2")?;
        context
            .set_cipher_list("PSK-AES128-GCM-SHA256")
            .context("failed to configure the Hue DTLS cipher")?;
        context.set_options(SslOptions::NO_QUERY_MTU);
        context.set_psk_client_callback(move |_, _, identity_buffer, psk_buffer| {
            if identity.len() + 1 > identity_buffer.len() || psk.len() > psk_buffer.len() {
                return Ok(0);
            }
            identity_buffer[..identity.len()].copy_from_slice(&identity);
            identity_buffer[identity.len()] = 0;
            psk_buffer[..psk.len()].copy_from_slice(&psk);
            Ok(psk.len())
        });

        let bind_address = match address {
            IpAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
            IpAddr::V6(_) => SocketAddr::from(([0_u16; 8], 0)),
        };
        let socket = UdpSocket::bind(bind_address).context("failed to bind Hue UDP socket")?;
        socket
            .connect(SocketAddr::new(address, ENTERTAINMENT_PORT))
            .context("failed to connect Hue UDP socket")?;
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set Hue UDP timeout")?;
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .context("failed to set Hue UDP timeout")?;

        let mut ssl = Ssl::new(&context.build()).context("failed to create DTLS session")?;
        ssl.set_connect_state();
        ssl.set_mtu(1200).context("failed to set DTLS MTU")?;
        let mut stream = openssl::ssl::SslStream::new(ssl, UdpConnection(socket))
            .context("failed to create Hue DTLS stream")?;
        stream
            .connect()
            .map_err(|_| anyhow!("Hue DTLS handshake failed"))?;
        Ok(Box::new(stream))
    }
}

#[derive(Clone)]
pub struct HueClient {
    candidate: BridgeCandidate,
    control: Arc<dyn ControlTransport>,
    credentials: Arc<dyn CredentialStore>,
    stream_connector: Arc<dyn StreamConnector>,
}

impl HueClient {
    pub fn new(candidate: BridgeCandidate) -> Result<Self> {
        let candidate = normalize_candidate(candidate)?;
        let control = Arc::new(HttpsControlTransport::new(&candidate)?);
        Ok(Self {
            candidate,
            control,
            credentials: Arc::new(SecretServiceCredentialStore),
            stream_connector: Arc::new(OpenSslDtlsConnector),
        })
    }

    pub fn with_transports(
        candidate: BridgeCandidate,
        control: Arc<dyn ControlTransport>,
        credentials: Arc<dyn CredentialStore>,
        stream_connector: Arc<dyn StreamConnector>,
    ) -> Result<Self> {
        let candidate = normalize_candidate(candidate)?;
        Ok(Self {
            candidate,
            control,
            credentials,
            stream_connector,
        })
    }

    #[must_use]
    pub fn candidate(&self) -> &BridgeCandidate {
        &self.candidate
    }

    pub async fn discover() -> Result<Vec<BridgeCandidate>> {
        let local = match tokio::task::spawn_blocking(discover_local).await {
            Ok(Ok(bridges)) => bridges,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "local Hue discovery failed; trying cloud discovery");
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(error = %error, "local Hue discovery task failed; trying cloud discovery");
                Vec::new()
            }
        };
        if !local.is_empty() {
            return Ok(local);
        }
        discover_cloud().await
    }

    pub async fn pair(&self) -> Result<PairAttempt> {
        // Registration is deliberately not retried: duplicate POSTs can mint keys.
        let response = self
            .execute(ControlRequest {
                method: ControlMethod::Post,
                path: "/api".to_owned(),
                application_key: None,
                body: Some(serde_json::json!({
                    "devicetype": "omarchy-lightsync#desktop",
                    "generateclientkey": true
                })),
            })
            .await?;
        ensure_http_success(&response, "pairing")?;
        let entries: Vec<RegisterResponse> = serde_json::from_value(response.body)
            .context("Hue bridge returned an invalid pairing response")?;
        match entries
            .into_iter()
            .next()
            .context("empty Hue pairing response")?
        {
            RegisterResponse::Success { success } => {
                let credentials = BridgeCredentials::new(success.username, success.clientkey)?;
                self.credentials
                    .store(&self.candidate.id, &credentials)
                    .await?;
                Ok(PairAttempt::Paired)
            }
            RegisterResponse::Error { error } if error.error_type == 101 => {
                Ok(PairAttempt::WaitingForLinkButton)
            }
            RegisterResponse::Error { error } => {
                bail!("Hue pairing failed (bridge error {})", error.error_type)
            }
        }
    }

    pub async fn forget_credentials(&self) -> Result<()> {
        self.credentials.forget(&self.candidate.id).await
    }

    pub async fn verify_authorization(&self) -> Result<()> {
        let credentials = self.load_credentials().await?;
        let bridges: Vec<BridgeResource> = self
            .clip_get("/clip/v2/resource/bridge", credentials.application_key())
            .await?;
        let bridge = bridges
            .first()
            .context("Hue bridge identity response is empty")?;
        validate_bridge_identity(&self.candidate.id, &bridge.bridge_id)
    }

    pub async fn areas(&self) -> Result<Vec<EntertainmentArea>> {
        let credentials = self.load_credentials().await?;
        let areas: Vec<EntertainmentConfiguration> = self
            .clip_get(
                "/clip/v2/resource/entertainment_configuration",
                credentials.application_key(),
            )
            .await?;
        Ok(areas.into_iter().map(EntertainmentArea::from).collect())
    }

    pub async fn area_channels(&self, area_id: &str) -> Result<Vec<ChannelPosition>> {
        let credentials = self.load_credentials().await?;
        let area = self.area(area_id, credentials.application_key()).await?;
        Ok(area
            .channels
            .into_iter()
            .map(|channel| ChannelPosition {
                channel_id: channel.channel_id,
                x: channel.position.x,
                y: channel.position.y,
                z: channel.position.z,
            })
            .collect())
    }

    pub async fn snapshot_area_lights(&self, area_id: &str) -> Result<LightSnapshot> {
        let credentials = self.load_credentials().await?;
        self.snapshot_area_lights_with_key(area_id, credentials.application_key())
            .await
    }

    pub async fn restore_light_snapshot(&self, snapshot: LightSnapshot) -> Result<()> {
        let credentials = self.load_credentials().await?;
        self.restore_with_key(snapshot, credentials.application_key())
            .await
    }

    pub async fn start_area(&self, area_id: &str) -> Result<()> {
        let credentials = self.load_credentials().await?;
        let area = self.area(area_id, credentials.application_key()).await?;
        if area.status == "active" {
            bail!("Hue Entertainment area is already active");
        }
        self.set_area_action(area_id, credentials.application_key(), "start")
            .await
    }

    pub async fn stop_area(&self, area_id: &str) -> Result<()> {
        let credentials = self.load_credentials().await?;
        self.set_area_action(area_id, credentials.application_key(), "stop")
            .await
    }

    pub async fn open_stream(&self, area_id: &str) -> Result<HueStreamSession> {
        self.open_stream_with_restore(area_id, true).await
    }

    pub async fn open_stream_with_restore(
        &self,
        area_id: &str,
        restore_on_stop: bool,
    ) -> Result<HueStreamSession> {
        let credentials = self.load_credentials().await?;
        let area = self.area(area_id, credentials.application_key()).await?;
        if area.status == "active" {
            bail!("Hue Entertainment area is already active; refusing to take it over");
        }
        if area.channels.is_empty() {
            bail!("Hue Entertainment area has no channels");
        }
        let psk = hex::decode(&credentials.client_key)
            .context("stored Hue Entertainment key is invalid")?;
        let snapshot = self
            .snapshot_area_lights_with_area(&area, credentials.application_key())
            .await?;
        // Once activation may have reached the bridge, this guard owns cleanup until
        // a session is returned. SIGKILL and power loss cannot run process cleanup.
        let mut activation = AreaActivationGuard::new(
            self.clone(),
            area_id.to_owned(),
            credentials.application_key.clone(),
            snapshot,
        );
        activation.arm();
        self.set_area_action(area_id, credentials.application_key(), "start")
            .await?;

        let channels = area
            .channels
            .iter()
            .map(|channel| ChannelPosition {
                channel_id: channel.channel_id,
                x: channel.position.x,
                y: channel.position.y,
                z: channel.position.z,
            })
            .collect();
        let mailbox = Arc::new(StreamMailbox::default());
        let (ready_sender, ready_receiver) = sync_channel(1);
        let connector = Arc::clone(&self.stream_connector);
        let address = self.candidate.address;
        let worker_area_id = area_id.to_owned();
        let identity = credentials.application_key.clone();
        let worker_mailbox = Arc::clone(&mailbox);
        let worker = tokio::task::spawn_blocking(move || {
            run_stream_worker(
                connector,
                address,
                &identity,
                &psk,
                &worker_area_id,
                worker_mailbox,
                ready_sender,
            )
        });
        let ready =
            tokio::task::spawn_blocking(move || ready_receiver.recv_timeout(REQUEST_TIMEOUT))
                .await
                .context("DTLS readiness task failed")?
                .context("timed out establishing Hue Entertainment DTLS")?;
        if let Err(error) = ready {
            activation.cleanup().await;
            bail!(error);
        }
        let snapshot = activation.disarm(restore_on_stop);
        Ok(HueStreamSession {
            mailbox,
            worker: Some(worker),
            client: self.clone(),
            area_id: area_id.to_owned(),
            application_key: credentials.application_key,
            snapshot,
            channels,
            cleanup_on_drop: true,
        })
    }

    async fn load_credentials(&self) -> Result<BridgeCredentials> {
        self.credentials.load(&self.candidate.id).await
    }

    async fn execute(&self, request: ControlRequest) -> Result<ControlResponse> {
        self.control.execute(request).await
    }

    async fn execute_safe(&self, request: ControlRequest) -> Result<ControlResponse> {
        let mut last_error = None;
        let mut last_response = None;
        for attempt in 0..SAFE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(150 * attempt as u64)).await;
            }
            match self.control.execute(request.clone()).await {
                Ok(response) if !matches!(response.status, 429 | 502 | 503 | 504) => {
                    return Ok(response);
                }
                Ok(response) => last_response = Some(response),
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(response) = last_response {
            Ok(response)
        } else {
            Err(last_error.context("Hue request failed without an error")?)
        }
    }

    async fn clip_get<T: DeserializeOwned>(&self, path: &str, key: &str) -> Result<Vec<T>> {
        let response = self
            .execute_safe(ControlRequest {
                method: ControlMethod::Get,
                path: path.to_owned(),
                application_key: Some(key.to_owned()),
                body: None,
            })
            .await?;
        if response.status == StatusCode::FORBIDDEN.as_u16() {
            bail!("Hue authorization was revoked; pair the bridge again");
        }
        ensure_http_success(&response, "resource request")?;
        parse_clip(response.body, "Hue resource request")
    }

    async fn area(&self, area_id: &str, key: &str) -> Result<EntertainmentConfiguration> {
        validate_resource_id(area_id)?;
        self.clip_get::<EntertainmentConfiguration>(
            &format!("/clip/v2/resource/entertainment_configuration/{area_id}"),
            key,
        )
        .await?
        .into_iter()
        .next()
        .context("selected Hue Entertainment area no longer exists")
    }

    async fn set_area_action(&self, area_id: &str, key: &str, action: &str) -> Result<()> {
        validate_resource_id(area_id)?;
        let response = self
            .execute_safe(ControlRequest {
                method: ControlMethod::Put,
                path: format!("/clip/v2/resource/entertainment_configuration/{area_id}"),
                application_key: Some(key.to_owned()),
                body: Some(serde_json::json!({"action": action})),
            })
            .await?;
        ensure_http_success(&response, "Entertainment action")?;
        let _: Vec<ResourceIdentifier> = parse_clip(response.body, "Entertainment action")?;
        Ok(())
    }

    async fn snapshot_area_lights_with_key(
        &self,
        area_id: &str,
        key: &str,
    ) -> Result<LightSnapshot> {
        let area = self.area(area_id, key).await?;
        self.snapshot_area_lights_with_area(&area, key).await
    }

    async fn snapshot_area_lights_with_area(
        &self,
        area: &EntertainmentConfiguration,
        key: &str,
    ) -> Result<LightSnapshot> {
        let mut light_ids: HashSet<String> = area
            .light_services
            .iter()
            .map(|service| service.rid.clone())
            .collect();
        if light_ids.is_empty() {
            let service_ids: HashSet<&str> = area
                .channels
                .iter()
                .flat_map(|channel| &channel.members)
                .map(|member| member.service.rid.as_str())
                .collect();
            let entertainment: Vec<EntertainmentResource> = self
                .clip_get("/clip/v2/resource/entertainment", key)
                .await?;
            let lights: Vec<LightResource> = self.clip_get("/clip/v2/resource/light", key).await?;
            let lights_by_owner: HashMap<&str, &str> = lights
                .iter()
                .filter_map(|light| {
                    light
                        .owner
                        .as_ref()
                        .map(|owner| (owner.rid.as_str(), light.id.as_str()))
                })
                .collect();
            for service in entertainment
                .iter()
                .filter(|service| service_ids.contains(service.id.as_str()))
            {
                if let Some(reference) = &service.renderer_reference {
                    light_ids.insert(reference.rid.clone());
                } else if let Some(light_id) = service
                    .owner
                    .as_ref()
                    .and_then(|owner| lights_by_owner.get(owner.rid.as_str()))
                {
                    light_ids.insert((*light_id).to_owned());
                }
            }
        }
        if light_ids.is_empty() {
            bail!("Hue Entertainment area exposes no restorable lights");
        }

        let lights: Vec<LightResource> = self.clip_get("/clip/v2/resource/light", key).await?;
        let unavailable: HashSet<String> = self
            .clip_get::<ZigbeeConnectivity>("/clip/v2/resource/zigbee_connectivity", key)
            .await?
            .into_iter()
            .filter(|resource| resource.status != "connected")
            .map(|resource| resource.owner.rid)
            .collect();
        let matching: Vec<_> = lights
            .into_iter()
            .filter(|light| light_ids.contains(&light.id))
            .collect();
        if matching.len() != light_ids.len() {
            bail!("could not read every participating Hue light");
        }
        let lights = matching
            .into_iter()
            .filter(|light| {
                light
                    .owner
                    .as_ref()
                    .is_none_or(|owner| !unavailable.contains(&owner.rid))
            })
            .map(|light| LightRestore {
                id: light.id.clone(),
                state: light.restore_state(),
            })
            .collect::<Vec<_>>();
        if lights.is_empty() {
            bail!("Hue Entertainment area has no connected lights to restore");
        }
        Ok(LightSnapshot { lights })
    }

    async fn restore_with_key(&self, snapshot: LightSnapshot, key: &str) -> Result<()> {
        let mut failures = Vec::new();
        for light in snapshot.lights {
            let request = ControlRequest {
                method: ControlMethod::Put,
                path: format!("/clip/v2/resource/light/{}", light.id),
                application_key: Some(key.to_owned()),
                body: Some(light.state),
            };
            match self.execute_safe(request).await.and_then(|response| {
                ensure_http_success(&response, "light restoration")?;
                parse_clip::<ResourceIdentifier>(response.body, "light restoration")?;
                Ok(())
            }) {
                Ok(()) => {}
                Err(_) => failures.push(light.id),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("could not restore {} Hue light(s)", failures.len())
        }
    }
}

#[must_use = "call stop() to deactivate the area and restore participating lights"]
pub struct HueStreamSession {
    mailbox: Arc<StreamMailbox>,
    worker: Option<tokio::task::JoinHandle<Result<()>>>,
    client: HueClient,
    area_id: String,
    application_key: String,
    snapshot: Option<LightSnapshot>,
    channels: Vec<ChannelPosition>,
    cleanup_on_drop: bool,
}

impl HueStreamSession {
    #[must_use]
    pub fn channels(&self) -> &[ChannelPosition] {
        &self.channels
    }

    pub fn send(&self, colors: Vec<ChannelColor>) -> Result<()> {
        if self.mailbox.stopped.load(Ordering::Acquire) {
            let message = self
                .mailbox
                .failure
                .lock()
                .map_err(|_| anyhow!("Hue stream failure lock poisoned"))?
                .clone()
                .unwrap_or_else(|| "Hue Entertainment stream stopped".to_owned());
            bail!(message);
        }
        *self
            .mailbox
            .latest
            .lock()
            .map_err(|_| anyhow!("Hue stream mailbox lock poisoned"))? = Some(colors);
        self.mailbox.wake.notify_one();
        Ok(())
    }

    pub async fn stop(mut self) -> Result<()> {
        self.mailbox.stopped.store(true, Ordering::Release);
        self.mailbox.wake.notify_all();
        let worker_result = match self.worker.take() {
            Some(worker) => match tokio::time::timeout(STREAM_STOP_TIMEOUT, worker).await {
                Ok(result) => result.context("Hue Entertainment worker failed")?,
                Err(_) => Err(anyhow!("Hue Entertainment worker did not stop in time")),
            },
            None => Ok(()),
        };
        let stop_result = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            self.client
                .set_area_action(&self.area_id, &self.application_key, "stop"),
        )
        .await
        .map_err(|_| anyhow!("timed out deactivating Hue Entertainment area"))?;
        let restore_result = if let Some(snapshot) = self.snapshot.clone() {
            tokio::time::timeout(
                CLEANUP_TIMEOUT,
                self.client
                    .restore_with_key(snapshot, &self.application_key),
            )
            .await
            .map_err(|_| anyhow!("timed out restoring Hue lights"))?
        } else {
            Ok(())
        };
        self.snapshot = None;
        self.cleanup_on_drop = false;
        worker_result.and(stop_result).and(restore_result)
    }
}

impl Drop for HueStreamSession {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        self.mailbox.stopped.store(true, Ordering::Release);
        self.mailbox.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        let client = self.client.clone();
        let area_id = self.area_id.clone();
        let application_key = self.application_key.clone();
        let snapshot = self.snapshot.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    CLEANUP_TIMEOUT,
                    client.set_area_action(&area_id, &application_key, "stop"),
                )
                .await;
                if let Some(snapshot) = snapshot {
                    let _ = tokio::time::timeout(
                        CLEANUP_TIMEOUT,
                        client.restore_with_key(snapshot, &application_key),
                    )
                    .await;
                }
            });
        }
    }
}

#[derive(Default)]
struct StreamMailbox {
    latest: Mutex<Option<Vec<ChannelColor>>>,
    wake: Condvar,
    stopped: AtomicBool,
    failure: Mutex<Option<String>>,
}

fn run_stream_worker(
    connector: Arc<dyn StreamConnector>,
    address: IpAddr,
    identity: &str,
    psk: &[u8],
    area_id: &str,
    mailbox: Arc<StreamMailbox>,
    ready: SyncSender<Result<(), String>>,
) -> Result<()> {
    let mut stream = match connector.connect(address, identity, psk) {
        Ok(stream) => {
            let _ = ready.send(Ok(()));
            stream
        }
        Err(_) => {
            let message = "failed to establish Hue Entertainment DTLS".to_owned();
            let _ = ready.send(Err(message.clone()));
            bail!(message)
        }
    };
    let mut sequence = 0_u8;
    loop {
        let colors = {
            let mut latest = mailbox
                .latest
                .lock()
                .map_err(|_| anyhow!("Hue stream mailbox lock poisoned"))?;
            while latest.is_none() && !mailbox.stopped.load(Ordering::Acquire) {
                latest = mailbox
                    .wake
                    .wait(latest)
                    .map_err(|_| anyhow!("Hue stream mailbox lock poisoned"))?;
            }
            if mailbox.stopped.load(Ordering::Acquire) {
                return Ok(());
            }
            latest.take().context("Hue stream mailbox woke empty")?
        };
        if let Err(error) = stream
            .write_all(&encode_rgb_packet(area_id, sequence, &colors)?)
            .context("failed to send Hue Entertainment frame")
        {
            if let Ok(mut failure) = mailbox.failure.lock() {
                *failure = Some(error.to_string());
            }
            mailbox.stopped.store(true, Ordering::Release);
            mailbox.wake.notify_all();
            return Err(error);
        }
        sequence = sequence.wrapping_add(1);
    }
}

struct AreaActivationGuard {
    client: HueClient,
    area_id: String,
    application_key: String,
    snapshot: Option<LightSnapshot>,
    armed: bool,
}

impl AreaActivationGuard {
    fn new(
        client: HueClient,
        area_id: String,
        application_key: String,
        snapshot: LightSnapshot,
    ) -> Self {
        Self {
            client,
            area_id,
            application_key,
            snapshot: Some(snapshot),
            armed: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(mut self, restore_on_stop: bool) -> Option<LightSnapshot> {
        self.armed = false;
        if restore_on_stop {
            self.snapshot.take()
        } else {
            None
        }
    }

    async fn cleanup(mut self) {
        self.armed = false;
        let _ = tokio::time::timeout(
            CLEANUP_TIMEOUT,
            self.client
                .set_area_action(&self.area_id, &self.application_key, "stop"),
        )
        .await;
        if let Some(snapshot) = self.snapshot.take() {
            let _ = tokio::time::timeout(
                CLEANUP_TIMEOUT,
                self.client
                    .restore_with_key(snapshot, &self.application_key),
            )
            .await;
        }
    }
}

impl Drop for AreaActivationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let client = self.client.clone();
        let area_id = self.area_id.clone();
        let application_key = self.application_key.clone();
        let snapshot = self.snapshot.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    CLEANUP_TIMEOUT,
                    client.set_area_action(&area_id, &application_key, "stop"),
                )
                .await;
                if let Some(snapshot) = snapshot {
                    let _ = tokio::time::timeout(
                        CLEANUP_TIMEOUT,
                        client.restore_with_key(snapshot, &application_key),
                    )
                    .await;
                }
            });
        }
    }
}

pub fn encode_rgb_packet(area_id: &str, sequence: u8, colors: &[ChannelColor]) -> Result<Vec<u8>> {
    let area_id = uuid::Uuid::parse_str(area_id).context("Entertainment area ID is not a UUID")?;
    let area_id = area_id.hyphenated().to_string();
    let mut seen = HashSet::new();
    if colors.iter().any(|color| !seen.insert(color.channel_id)) {
        bail!("Hue Entertainment frame contains a duplicate channel");
    }
    let mut packet = Vec::with_capacity(52 + colors.len() * 7);
    packet.extend_from_slice(b"HueStream");
    packet.extend_from_slice(&[0x02, 0x00, sequence, 0x00, 0x00, 0x00, 0x00]);
    packet.extend_from_slice(area_id.as_bytes());
    for color in colors {
        packet.push(color.channel_id);
        packet.extend_from_slice(&color.red.to_be_bytes());
        packet.extend_from_slice(&color.green.to_be_bytes());
        packet.extend_from_slice(&color.blue.to_be_bytes());
    }
    Ok(packet)
}

struct UdpConnection(UdpSocket);

impl Read for UdpConnection {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.recv(buffer)
    }
}

impl Write for UdpConnection {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.send(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_candidate(candidate: &BridgeCandidate) -> Result<()> {
    normalize_bridge_id(&candidate.id)?;
    if candidate.address.is_unspecified() || candidate.address.is_multicast() {
        bail!("Hue bridge address is not usable");
    }
    Ok(())
}

fn normalize_candidate(mut candidate: BridgeCandidate) -> Result<BridgeCandidate> {
    validate_candidate(&candidate)?;
    candidate.id = normalize_bridge_id(&candidate.id)?;
    Ok(candidate)
}

fn normalize_bridge_id(id: &str) -> Result<String> {
    if id.len() != 16 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Hue bridge ID must contain exactly 16 hexadecimal characters");
    }
    Ok(id.to_ascii_lowercase())
}

fn validate_bridge_identity(expected: &str, actual: &str) -> Result<()> {
    let expected = normalize_bridge_id(expected)?;
    let actual = normalize_bridge_id(actual)?;
    if expected != actual {
        bail!("Hue bridge identity does not match the configured bridge");
    }
    Ok(())
}

fn validate_resource_id(id: &str) -> Result<()> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .context("Hue resource ID is not a UUID")
}

fn ensure_http_success(response: &ControlResponse, operation: &str) -> Result<()> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        bail!(
            "Hue {operation} failed with HTTP status {}",
            response.status
        )
    }
}

fn parse_clip<T: DeserializeOwned>(body: Value, operation: &str) -> Result<Vec<T>> {
    let envelope: ClipEnvelope<T> =
        serde_json::from_value(body).context("Hue bridge returned malformed CLIP data")?;
    if !envelope.errors.is_empty() {
        bail!(
            "{operation} failed with {} Hue API error(s)",
            envelope.errors.len()
        );
    }
    Ok(envelope.data)
}

fn discover_local() -> Result<Vec<BridgeCandidate>> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    const SERVICE_TYPE: &str = "_hue._tcp.local.";
    let daemon = ServiceDaemon::new().context("failed to start local mDNS discovery")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("failed to browse for Hue bridges")?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut bridges = HashMap::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let id = info
                    .get_property_val_str("bridgeid")
                    .or_else(|| info.get_property_val_str("id"))
                    .and_then(|id| normalize_bridge_id(id).ok());
                let Some(id) = id else { continue };
                let address = info
                    .get_addresses()
                    .iter()
                    .map(mdns_sd::ScopedIp::to_ip_addr)
                    .filter(|address| !address.is_loopback() && !address.is_unspecified())
                    .min_by_key(|address| u8::from(address.is_ipv6()));
                if let Some(address) = address {
                    bridges.insert(
                        id.clone(),
                        BridgeCandidate {
                            id,
                            address,
                            name: info
                                .get_property_val_str("name")
                                .unwrap_or("Hue Bridge")
                                .to_owned(),
                        },
                    );
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    let mut bridges: Vec<_> = bridges.into_values().collect();
    bridges.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(bridges)
}

async fn discover_cloud() -> Result<Vec<BridgeCandidate>> {
    let response = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("failed to build Hue discovery client")?
        .get(DISCOVERY_URL)
        .send()
        .await
        .context("Hue discovery service is unavailable")?
        .error_for_status()
        .context("Hue discovery service returned an error")?
        .json::<Vec<DiscoveryBridge>>()
        .await
        .context("Hue discovery service returned invalid data")?;
    response
        .into_iter()
        .map(|bridge| {
            Ok(BridgeCandidate {
                id: normalize_bridge_id(&bridge.id)?,
                address: bridge.internalipaddress,
                name: "Hue Bridge".to_owned(),
            })
        })
        .collect()
}

impl From<EntertainmentConfiguration> for EntertainmentArea {
    fn from(area: EntertainmentConfiguration) -> Self {
        Self {
            id: area.id,
            name: area.metadata.name,
            channels: u16::try_from(area.channels.len()).unwrap_or(u16::MAX),
            active: area.status == "active",
            active_streamer: area.active_streamer.map(|streamer| streamer.rid),
        }
    }
}

#[derive(Deserialize)]
struct DiscoveryBridge {
    id: String,
    internalipaddress: IpAddr,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RegisterResponse {
    Success { success: RegisterSuccess },
    Error { error: RegisterError },
}

#[derive(Deserialize)]
struct RegisterSuccess {
    username: String,
    clientkey: String,
}

#[derive(Deserialize)]
struct RegisterError {
    #[serde(rename = "type")]
    error_type: u16,
}

#[derive(Deserialize)]
struct ClipEnvelope<T> {
    #[serde(default)]
    errors: Vec<Value>,
    data: Vec<T>,
}

#[derive(Deserialize)]
struct BridgeResource {
    bridge_id: String,
}

#[derive(Clone, Deserialize)]
struct EntertainmentConfiguration {
    id: String,
    metadata: Metadata,
    status: String,
    active_streamer: Option<ResourceIdentifier>,
    #[serde(default)]
    channels: Vec<EntertainmentChannel>,
    #[serde(default)]
    light_services: Vec<ResourceIdentifier>,
}

#[derive(Clone, Deserialize)]
struct EntertainmentChannel {
    channel_id: u8,
    position: EntertainmentPosition,
    #[serde(default)]
    members: Vec<EntertainmentMember>,
}

#[derive(Clone, Deserialize)]
struct EntertainmentMember {
    service: ResourceIdentifier,
}

#[derive(Clone, Deserialize)]
struct EntertainmentPosition {
    x: f32,
    #[serde(default)]
    y: f32,
    z: f32,
}

#[derive(Clone, Deserialize)]
struct Metadata {
    name: String,
}

#[derive(Clone, Deserialize)]
struct ResourceIdentifier {
    rid: String,
}

#[derive(Deserialize)]
struct EntertainmentResource {
    id: String,
    owner: Option<ResourceIdentifier>,
    renderer_reference: Option<ResourceIdentifier>,
}

#[derive(Deserialize)]
struct ZigbeeConnectivity {
    owner: ResourceIdentifier,
    status: String,
}

#[derive(Deserialize)]
struct LightResource {
    id: String,
    owner: Option<ResourceIdentifier>,
    on: LightOn,
    dimming: Option<LightDimming>,
    color: Option<LightColor>,
    color_temperature: Option<LightColorTemperature>,
    gradient: Option<LightGradient>,
}

impl LightResource {
    fn restore_state(&self) -> Value {
        let mut state = serde_json::Map::new();
        state.insert("on".to_owned(), serde_json::json!({"on": self.on.on}));
        if let Some(dimming) = &self.dimming {
            state.insert(
                "dimming".to_owned(),
                serde_json::json!({"brightness": dimming.brightness}),
            );
        }
        if let Some(gradient) = self
            .gradient
            .as_ref()
            .filter(|value| !value.points.is_empty())
        {
            state.insert("gradient".to_owned(), serde_json::json!(gradient));
        } else if let Some(mirek) = self
            .color_temperature
            .as_ref()
            .filter(|value| value.mirek_valid)
            .and_then(|value| value.mirek)
        {
            state.insert(
                "color_temperature".to_owned(),
                serde_json::json!({"mirek": mirek}),
            );
        } else if let Some(color) = &self.color {
            state.insert("color".to_owned(), serde_json::json!({"xy": color.xy}));
        }
        Value::Object(state)
    }
}

#[derive(Deserialize)]
struct LightOn {
    on: bool,
}

#[derive(Deserialize)]
struct LightDimming {
    brightness: f64,
}

#[derive(Serialize, Deserialize)]
struct LightColor {
    xy: LightXy,
}

#[derive(Serialize, Deserialize)]
struct LightXy {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct LightColorTemperature {
    mirek: Option<u16>,
    #[serde(default)]
    mirek_valid: bool,
}

#[derive(Serialize, Deserialize)]
struct LightGradient {
    points: Vec<LightGradientPoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct LightGradientPoint {
    color: LightColor,
}

pub mod fake {
    use std::collections::{HashMap, VecDeque};
    use std::io::Write;
    use std::net::IpAddr;
    use std::sync::{Arc, Mutex};

    use anyhow::{Context, Result, bail};
    use async_trait::async_trait;

    use super::{
        BridgeCredentials, ControlRequest, ControlResponse, ControlTransport, CredentialStore,
        StreamConnector,
    };

    #[derive(Default)]
    pub struct ScriptedControlTransport {
        responses: Mutex<VecDeque<Result<ControlResponse, String>>>,
        requests: Mutex<Vec<ControlRequest>>,
    }

    impl ScriptedControlTransport {
        pub fn push_response(&self, response: ControlResponse) {
            self.responses
                .lock()
                .expect("fake response lock poisoned")
                .push_back(Ok(response));
        }

        pub fn push_error(&self, message: impl Into<String>) {
            self.responses
                .lock()
                .expect("fake response lock poisoned")
                .push_back(Err(message.into()));
        }

        pub fn requests(&self) -> Vec<ControlRequest> {
            self.requests
                .lock()
                .expect("fake request lock poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl ControlTransport for ScriptedControlTransport {
        async fn execute(&self, request: ControlRequest) -> Result<ControlResponse> {
            self.requests
                .lock()
                .map_err(|_| anyhow::anyhow!("fake request lock poisoned"))?
                .push(request);
            match self
                .responses
                .lock()
                .map_err(|_| anyhow::anyhow!("fake response lock poisoned"))?
                .pop_front()
                .context("fake control response queue is empty")?
            {
                Ok(response) => Ok(response),
                Err(message) => bail!(message),
            }
        }
    }

    #[derive(Default)]
    pub struct MemoryCredentialStore {
        values: Mutex<HashMap<String, BridgeCredentials>>,
    }

    #[async_trait]
    impl CredentialStore for MemoryCredentialStore {
        async fn load(&self, bridge_id: &str) -> Result<BridgeCredentials> {
            self.values
                .lock()
                .map_err(|_| anyhow::anyhow!("fake credential lock poisoned"))?
                .get(bridge_id)
                .cloned()
                .context("fake credentials not found")
        }

        async fn store(&self, bridge_id: &str, credentials: &BridgeCredentials) -> Result<()> {
            self.values
                .lock()
                .map_err(|_| anyhow::anyhow!("fake credential lock poisoned"))?
                .insert(bridge_id.to_owned(), credentials.clone());
            Ok(())
        }

        async fn forget(&self, bridge_id: &str) -> Result<()> {
            self.values
                .lock()
                .map_err(|_| anyhow::anyhow!("fake credential lock poisoned"))?
                .remove(bridge_id);
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    pub struct RecordingStreamConnector {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl RecordingStreamConnector {
        pub fn bytes(&self) -> Vec<u8> {
            self.bytes
                .lock()
                .expect("fake stream lock poisoned")
                .clone()
        }
    }

    impl StreamConnector for RecordingStreamConnector {
        fn connect(
            &self,
            _address: IpAddr,
            _identity: &str,
            _psk: &[u8],
        ) -> Result<Box<dyn Write + Send>> {
            Ok(Box::new(RecordingWriter(Arc::clone(&self.bytes))))
        }
    }

    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("fake stream lock poisoned"))?
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_identity_validation_is_strict_and_case_insensitive() {
        assert!(validate_bridge_identity("001788FFFE123456", "001788fffe123456").is_ok());
        assert!(validate_bridge_identity("001788fffe123456", "001788fffe654321").is_err());
        assert!(validate_bridge_identity("001788fffe12345z", "001788fffe123456").is_err());
    }

    #[test]
    fn clip_v2_area_parses_status_and_sparse_channels() {
        let areas: Vec<EntertainmentConfiguration> = parse_clip(
            serde_json::json!({
                "errors": [],
                "data": [{
                    "id": "ac342fb3-c480-4555-8e12-3a53e5f52344",
                    "metadata": {"name": "Desk"},
                    "status": "active",
                    "active_streamer": {"rid": "streamer", "rtype": "auth_v1"},
                    "channels": [
                        {"channel_id": 0, "position": {"x": -1.0, "y": 0.2, "z": 0.5}},
                        {"channel_id": 7, "position": {"x": 1.0, "z": -0.5}}
                    ]
                }]
            }),
            "test",
        )
        .expect("fixture parses");
        let area = EntertainmentArea::from(areas[0].clone());
        assert!(area.active);
        assert_eq!(area.active_streamer.as_deref(), Some("streamer"));
        assert_eq!(area.channels, 2);
        assert_eq!(areas[0].channels[1].channel_id, 7);
        assert_eq!(areas[0].channels[1].position.y, 0.0);
    }

    #[test]
    fn hue_stream_v2_packet_has_canonical_header_and_rgb16_data() {
        let packet = encode_rgb_packet(
            "AC342FB3-C480-4555-8E12-3A53E5F52344",
            9,
            &[ChannelColor {
                channel_id: 7,
                red: 0x1234,
                green: 0x5678,
                blue: 0x9abc,
            }],
        )
        .expect("packet encodes");
        assert_eq!(&packet[..9], b"HueStream");
        assert_eq!(&packet[9..16], &[2, 0, 9, 0, 0, 0, 0]);
        assert_eq!(&packet[16..52], b"ac342fb3-c480-4555-8e12-3a53e5f52344");
        assert_eq!(&packet[52..], &[7, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
    }

    #[test]
    fn hue_stream_rejects_duplicate_channels() {
        let color = ChannelColor {
            channel_id: 1,
            red: 0,
            green: 0,
            blue: 0,
        };
        assert!(
            encode_rgb_packet("ac342fb3-c480-4555-8e12-3a53e5f52344", 0, &[color, color]).is_err()
        );
    }

    #[test]
    fn restoration_payload_contains_only_writable_pre_stream_state() {
        let light: LightResource = serde_json::from_value(serde_json::json!({
            "id": "light-1",
            "owner": {"rid": "device-1", "rtype": "device"},
            "on": {"on": true},
            "dimming": {"brightness": 87.5, "min_dim_level": 0.2},
            "color": {"xy": {"x": 0.31, "y": 0.33}, "gamut_type": "C"},
            "color_temperature": {
                "mirek": 250,
                "mirek_valid": true,
                "mirek_schema": {"mirek_minimum": 153, "mirek_maximum": 500}
            }
        }))
        .expect("fixture parses");
        assert_eq!(
            light.restore_state(),
            serde_json::json!({
                "on": {"on": true},
                "dimming": {"brightness": 87.5},
                "color_temperature": {"mirek": 250}
            })
        );
    }

    #[test]
    fn credential_debug_output_is_redacted() {
        let credentials = BridgeCredentials::new("application-secret".into(), "0011".into())
            .expect("credentials are valid");
        let output = format!("{credentials:?}");
        assert!(!output.contains("application-secret"));
        assert!(!output.contains("0011"));
    }

    #[test]
    fn stream_mailbox_retains_only_the_latest_frame() {
        let mailbox = StreamMailbox::default();
        for red in [1, 2, 3] {
            *mailbox.latest.lock().expect("mailbox") = Some(vec![ChannelColor {
                channel_id: 0,
                red,
                green: 0,
                blue: 0,
            }]);
        }
        let latest = mailbox
            .latest
            .lock()
            .expect("mailbox")
            .take()
            .expect("frame");
        assert_eq!(latest[0].red, 3);
    }
}
