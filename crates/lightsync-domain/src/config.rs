use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} must be between {min} and {max}, got {value}")]
    OutOfRange {
        field: &'static str,
        min: u64,
        max: u64,
        value: u64,
    },
    #[error("unsupported schema version {0}")]
    UnsupportedSchema(u32),
    #[error("duplicate profile id {0}")]
    DuplicateProfile(ProfileId),
    #[error("active profile {0} does not exist")]
    UnknownActiveProfile(ProfileId),
    #[error("duplicate entertainment channel id {0}")]
    DuplicateChannel(u16),
}

macro_rules! string_id {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ValidationError::Empty { field: $field });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_id!(BridgeId, "bridge id");
string_id!(AreaId, "area id");
string_id!(DisplayId, "display id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(Uuid);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct NormalizedPoint {
    x: f32,
    y: f32,
}

impl NormalizedPoint {
    pub fn new(x: f32, y: f32) -> Result<Self, ValidationError> {
        validate_unit("x", x)?;
        validate_unit("y", y)?;
        Ok(Self { x, y })
    }

    pub const fn x(self) -> f32 {
        self.x
    }

    pub const fn y(self) -> f32 {
        self.y
    }
}

impl<'de> Deserialize<'de> for NormalizedPoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Point {
            x: f32,
            y: f32,
        }

        let point = Point::deserialize(deserializer)?;
        Self::new(point.x, point.y).map_err(serde::de::Error::custom)
    }
}

fn validate_unit(field: &'static str, value: f32) -> Result<(), ValidationError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ValidationError::OutOfRange {
            field,
            min: 0,
            max: 1,
            value: value.max(0.0) as u64,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Brightness(u8);

impl Brightness {
    pub const MAX: u8 = 100;

    pub fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            return Err(ValidationError::OutOfRange {
                field: "brightness",
                min: 0,
                max: Self::MAX as u64,
                value: value as u64,
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }
}

impl Default for Brightness {
    fn default() -> Self {
        Self(80)
    }
}

impl<'de> Deserialize<'de> for Brightness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeConfiguration {
    pub id: BridgeId,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl BridgeConfiguration {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.host.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "bridge host",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntertainmentChannel {
    pub id: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub position: NormalizedPoint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntertainmentArea {
    pub id: AreaId,
    pub name: String,
    #[serde(default)]
    pub channels: Vec<EntertainmentChannel>,
}

impl EntertainmentArea {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.name.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "entertainment area name",
            });
        }
        let mut ids = HashSet::new();
        for channel in &self.channels {
            if !ids.insert(channel.id) {
                return Err(ValidationError::DuplicateChannel(channel.id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    System,
    En,
    De,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureBackend {
    #[default]
    PortalPipewire,
    Grim,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    #[default]
    Video,
    Game,
    Music,
    Scene,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Intensity {
    Subtle,
    #[default]
    Moderate,
    High,
    Extreme,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSettings {
    pub mode: SyncMode,
    pub intensity: Intensity,
    pub brightness: Brightness,
    pub audio_reactive: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            mode: SyncMode::Video,
            intensity: Intensity::Moderate,
            brightness: Brightness::default(),
            audio_reactive: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub settings: SyncSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<AreaId>,
}

impl Profile {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.name.trim().is_empty() {
            return Err(ValidationError::Empty {
                field: "profile name",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub revision: u64,
    pub language: Language,
    pub capture_backend: CaptureBackend,
    pub sync: SyncSettings,
    pub restore_on_stop: bool,
    pub launch_at_login: bool,
    pub auto_start_sync: bool,
    pub selected_display: Option<DisplayId>,
    pub selected_area: Option<AreaId>,
    pub bridge: Option<BridgeConfiguration>,
    pub profiles: Vec<Profile>,
    pub active_profile: Option<ProfileId>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            revision: 0,
            language: Language::System,
            capture_backend: CaptureBackend::PortalPipewire,
            sync: SyncSettings::default(),
            restore_on_stop: true,
            launch_at_login: false,
            auto_start_sync: false,
            selected_display: None,
            selected_area: None,
            bridge: None,
            profiles: Vec::new(),
            active_profile: None,
        }
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchema(self.schema_version));
        }
        if let Some(bridge) = &self.bridge {
            bridge.validate()?;
        }
        let mut ids = HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if !ids.insert(profile.id) {
                return Err(ValidationError::DuplicateProfile(profile.id));
            }
        }
        if let Some(active) = self.active_profile
            && !ids.contains(&active)
        {
            return Err(ValidationError::UnknownActiveProfile(active));
        }
        Ok(())
    }

    pub fn apply_update(&mut self, update: ConfigUpdate) -> Result<(), ValidationError> {
        let mut next = self.clone();
        if let Some(value) = update.language {
            next.language = value;
        }
        if let Some(value) = update.capture_backend {
            next.capture_backend = value;
        }
        if let Some(value) = update.sync {
            next.sync = value;
        }
        if let Some(value) = update.restore_on_stop {
            next.restore_on_stop = value;
        }
        if let Some(value) = update.launch_at_login {
            next.launch_at_login = value;
        }
        if let Some(value) = update.auto_start_sync {
            next.auto_start_sync = value;
        }
        if let Some(value) = update.selected_display {
            next.selected_display = value;
        }
        if let Some(value) = update.selected_area {
            next.selected_area = value;
        }
        if let Some(value) = update.bridge {
            next.bridge = value;
        }
        next.revision = next.revision.saturating_add(1);
        next.validate()?;
        *self = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_backend: Option<CaptureBackend>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_on_stop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_at_login: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_start_sync: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nullable"
    )]
    pub selected_display: Option<Option<DisplayId>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nullable"
    )]
    pub selected_area: Option<Option<AreaId>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_nullable"
    )]
    pub bridge: Option<Option<BridgeConfiguration>>,
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_values_reject_invalid_json() {
        assert!(serde_json::from_str::<Brightness>("101").is_err());
        assert!(serde_json::from_str::<NormalizedPoint>(r#"{"x":-0.1,"y":0.5}"#).is_err());
        assert!(serde_json::from_str::<NormalizedPoint>(r#"{"x":null,"y":0.5}"#).is_err());
    }

    #[test]
    fn defaults_are_stable_and_valid() {
        let config = AppConfig::default();
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(config.sync.brightness.get(), 80);
        assert!(config.restore_on_stop);
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn config_round_trips_and_ignores_unknown_fields() {
        let mut value = serde_json::to_value(AppConfig::default()).expect("serialize config");
        value["future_option"] = serde_json::json!(true);
        let decoded: AppConfig = serde_json::from_value(value).expect("deserialize config");
        assert_eq!(decoded, AppConfig::default());
    }

    #[test]
    fn update_is_atomic_when_invalid() {
        let mut config = AppConfig::default();
        let missing = ProfileId::new();
        config.active_profile = Some(missing);
        let original = config.clone();
        let result = config.apply_update(ConfigUpdate {
            language: Some(Language::De),
            ..ConfigUpdate::default()
        });
        assert_eq!(result, Err(ValidationError::UnknownActiveProfile(missing)));
        assert_eq!(config, original);
    }

    #[test]
    fn update_distinguishes_missing_and_explicit_null() {
        let untouched: ConfigUpdate = serde_json::from_str("{}").expect("missing field");
        let cleared: ConfigUpdate =
            serde_json::from_str(r#"{"selected_area":null}"#).expect("null field");
        assert_eq!(untouched.selected_area, None);
        assert_eq!(cleared.selected_area, Some(None));
        assert_eq!(
            serde_json::to_string(&cleared).expect("serialize clear"),
            r#"{"selected_area":null}"#
        );
    }
}
