//! Download Profiles: named presets over the settings the app already has. A profile is not an engine or a
//! controller of its own: it holds only the values it changes, and everything it leaves out keeps the user's own
//! setting. The values in force are worked out by one rule ([`resolve`]) and handed to the existing controls (the
//! speed limiter's global layer, the queue's run slots, the engine's connection ceiling, Smart throttle's automatic
//! mode). The user's own settings are never overwritten, so "Default" is always exactly them.
//!
//! Precedence, from the base up (each layer only narrows or replaces what it names):
//!
//! ```text
//! the user's settings (Settings page)          -> base
//!   + the active profile's values              -> effective settings  (resolve)
//!   + per-download / per-type speed limits     -> the lowest limit wins (RateLimits, unchanged)
//!   + Smart throttle cap / gaming policy        -> its own layer, the lowest wins (unchanged)
//!   + queue gate (manual pause, schedule, game) -> decides what may start (unchanged)
//! ```
//!
//! Pure functions only (no I/O): the manager side is `manager/profiles.rs`.

use serde::{Deserialize, Serialize};

use crate::ManagerError;

/// Settings a profile may set. `None`: the user's own setting stays.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSettings {
    /// The global speed limit (bytes/s); `0` = no limit.
    pub speed_limit_bps: Option<u64>,
    /// Downloads running at once (1..=32).
    pub max_active: Option<u32>,
    /// Connections per download (1..=64; the engine still adapts below it and respects what the server allows).
    pub max_connections: Option<u32>,
    /// Smart throttle's automatic mode (give way while the internet is used, full speed when it is free).
    /// `true` also turns Smart throttle on; `false` holds Smart throttle off entirely (full speed: downloads never
    /// give way, not to browsing and not to a listed program).
    pub adaptive: Option<bool>,
}

/// The user's own values, or the values in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveSettings {
    /// `None` = no limit.
    pub speed_limit_bps: Option<u64>,
    pub max_active: u32,
    pub max_connections: u32,
    /// Smart throttle is on.
    pub smart_on: bool,
    /// Its automatic mode is on.
    pub adaptive: bool,
}

/// The built-in profiles (always there, never edited or deleted; duplicate one to change it).
pub const BUILT_IN: [&str; 4] = ["default", "fast", "balanced", "lowResource"];
pub const DEFAULT_ID: &str = "default";

/// Most custom profiles kept.
pub const MAX_CUSTOM: usize = 20;
pub const MAX_NAME_CHARS: usize = 40;
pub const MIN_ACTIVE: u32 = 1;
pub const MAX_ACTIVE: u32 = 32;
pub const MIN_CONNECTIONS: u32 = 1;
pub const MAX_CONNECTIONS: u32 = downloader_core::planner::MAX_CONNECTIONS as u32;
/// "Fast": a high connection ceiling; the adaptive controller only climbs while it brings more speed.
pub const FAST_CONNECTIONS: u32 = 64;
pub const LOW_ACTIVE: u32 = 1;
pub const LOW_CONNECTIONS: u32 = 4;

/// What a built-in profile sets.
pub fn built_in(id: &str) -> Option<ProfileSettings> {
    Some(match id {
        // The user's settings, as they are.
        "default" => ProfileSettings::default(),
        // Full speed, always: room for many connections (still adaptive), no speed limit, never giving way.
        "fast" => ProfileSettings {
            speed_limit_bps: Some(0),
            max_connections: Some(FAST_CONNECTIONS),
            adaptive: Some(false),
            ..Default::default()
        },
        // Full speed while the line is free, give way while it is used (Smart throttle's automatic mode).
        "balanced" => ProfileSettings { adaptive: Some(true), ..Default::default() },
        // One download at a time, few connections each.
        "lowResource" => ProfileSettings { max_active: Some(LOW_ACTIVE), max_connections: Some(LOW_CONNECTIONS), ..Default::default() },
        _ => return None,
    })
}

/// The one rule: a profile's value where it has one, the user's own everywhere else.
pub fn resolve(base: EffectiveSettings, p: ProfileSettings) -> EffectiveSettings {
    EffectiveSettings {
        speed_limit_bps: match p.speed_limit_bps {
            None => base.speed_limit_bps,
            Some(0) => None,
            Some(v) => Some(v),
        },
        max_active: p.max_active.unwrap_or(base.max_active),
        max_connections: p.max_connections.unwrap_or(base.max_connections),
        smart_on: p.adaptive != Some(false) && (base.smart_on || p.adaptive == Some(true)),
        adaptive: p.adaptive.unwrap_or(base.adaptive),
    }
}

/// Check a custom profile's values (the backend's rule; the UI's checks are only a convenience).
pub fn validate_settings(s: &ProfileSettings) -> Result<(), ManagerError> {
    if let Some(v) = s.speed_limit_bps.filter(|v| *v != 0) {
        crate::validate::speed_limit(Some(v))?;
    }
    if let Some(v) = s.max_active.filter(|v| !(MIN_ACTIVE..=MAX_ACTIVE).contains(v)) {
        return Err(ManagerError::invalid(format!("downloads at once must be between {MIN_ACTIVE} and {MAX_ACTIVE} (got {v})")));
    }
    if let Some(v) = s.max_connections.filter(|v| !(MIN_CONNECTIONS..=MAX_CONNECTIONS).contains(v)) {
        return Err(ManagerError::invalid(format!(
            "connections per download must be between {MIN_CONNECTIONS} and {MAX_CONNECTIONS} (got {v})"
        )));
    }
    Ok(())
}

/// A custom profile's name: trimmed, 1..=40 characters, no control characters.
pub fn validate_name(name: &str) -> Result<String, ManagerError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME_CHARS || n.chars().any(char::is_control) {
        return Err(ManagerError::invalid(format!("a profile name has 1 to {MAX_NAME_CHARS} characters")));
    }
    Ok(n.to_string())
}

/// A profile the user made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomProfile {
    /// `c1`, `c2`... (never reused).
    pub id: String,
    pub name: String,
    pub settings: ProfileSettings,
}

/// What is saved (one settings value, JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub version: u32,
    pub active: String,
    pub custom: Vec<CustomProfile>,
    pub next_id: u32,
}

impl Default for Saved {
    fn default() -> Self {
        Self { version: 1, active: DEFAULT_ID.into(), custom: Vec::new(), next_id: 1 }
    }
}

impl Saved {
    /// Read what was saved. Nothing saved, unreadable or from a newer version: the defaults (the user's settings,
    /// untouched). Invalid custom profiles are left out and an unknown active profile falls back to "Default";
    /// `repaired` then says the saved value should be written again.
    pub fn parse(text: Option<&str>) -> (Self, bool) {
        let Some(text) = text else { return (Self::default(), false) };
        let Ok(mut s) = serde_json::from_str::<Saved>(text) else { return (Self::default(), true) };
        if s.version != 1 {
            return (Self::default(), true);
        }
        let before = (s.custom.len(), s.active.clone());
        let mut seen = std::collections::HashSet::new();
        s.custom.retain(|c| {
            c.id.starts_with('c')
                && c.id[1..].parse::<u32>().is_ok()
                && seen.insert(c.id.clone())
                && validate_name(&c.name).is_ok()
                && validate_settings(&c.settings).is_ok()
        });
        s.custom.truncate(MAX_CUSTOM);
        let top = s.custom.iter().filter_map(|c| c.id[1..].parse::<u32>().ok()).max().unwrap_or(0);
        s.next_id = s.next_id.max(top + 1);
        if !s.exists(&s.active) {
            s.active = DEFAULT_ID.into();
        }
        let repaired = before != (s.custom.len(), s.active.clone());
        (s, repaired)
    }

    pub fn exists(&self, id: &str) -> bool {
        built_in(id).is_some() || self.custom.iter().any(|c| c.id == id)
    }

    /// The values of profile `id` (`None`: no such profile).
    pub fn settings(&self, id: &str) -> Option<ProfileSettings> {
        built_in(id).or_else(|| self.custom.iter().find(|c| c.id == id).map(|c| c.settings))
    }

    pub fn active_settings(&self) -> ProfileSettings {
        self.settings(&self.active).unwrap_or_default()
    }

    /// No two custom profiles with the same name (case ignored); `except` is the one being renamed.
    pub fn name_free(&self, name: &str, except: Option<&str>) -> bool {
        !self.custom.iter().any(|c| Some(c.id.as_str()) != except && c.name.to_lowercase() == name.to_lowercase())
    }

    fn custom(id: &str) -> impl Fn(&CustomProfile) -> bool + '_ {
        move |c| c.id == id
    }

    /// Apply one action. Nothing changes unless it is valid.
    pub fn apply(&mut self, action: ProfileAction) -> Result<(), ManagerError> {
        let no_such = || ManagerError::invalid("no such profile");
        let built_in_refused = || ManagerError::invalid("a built-in profile cannot be changed: duplicate it first");
        match action {
            ProfileAction::Select { id } => {
                if !self.exists(&id) {
                    return Err(no_such());
                }
                self.active = id;
            }
            ProfileAction::Create { name, settings } => {
                let name = validate_name(&name)?;
                validate_settings(&settings)?;
                self.add(name, settings)?;
            }
            ProfileAction::Duplicate { id, name } => {
                let settings = self.settings(&id).ok_or_else(no_such)?;
                let name = validate_name(&name)?;
                self.add(name, settings)?;
            }
            ProfileAction::Update { id, name, settings } => {
                if built_in(&id).is_some() {
                    return Err(built_in_refused());
                }
                let name = name.map(|n| validate_name(&n)).transpose()?;
                if let Some(s) = &settings {
                    validate_settings(s)?;
                }
                if let Some(n) = &name {
                    if !self.name_free(n, Some(&id)) {
                        return Err(ManagerError::invalid("another profile has that name"));
                    }
                }
                let c = self.custom.iter_mut().find(|c| c.id == id).ok_or_else(no_such)?;
                if let Some(n) = name {
                    c.name = n;
                }
                if let Some(s) = settings {
                    c.settings = s;
                }
            }
            ProfileAction::Delete { id } => {
                if built_in(&id).is_some() {
                    return Err(built_in_refused());
                }
                let at = self.custom.iter().position(Self::custom(&id)).ok_or_else(no_such)?;
                self.custom.remove(at);
                if self.active == id {
                    self.active = DEFAULT_ID.into(); // the fallback: the user's own settings
                }
            }
        }
        Ok(())
    }

    fn add(&mut self, name: String, settings: ProfileSettings) -> Result<(), ManagerError> {
        if self.custom.len() >= MAX_CUSTOM {
            return Err(ManagerError::invalid(format!("at most {MAX_CUSTOM} profiles of your own")));
        }
        if !self.name_free(&name, None) {
            return Err(ManagerError::invalid("another profile has that name"));
        }
        let id = format!("c{}", self.next_id);
        self.next_id += 1;
        self.custom.push(CustomProfile { id, name, settings });
        Ok(())
    }
}

/// A change to the profiles (`profile_action`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum ProfileAction {
    /// Make it the active profile (applied at once).
    Select {
        id: String,
    },
    Create {
        name: String,
        settings: ProfileSettings,
    },
    /// A copy of any profile (built-in included) as a profile of the user's own.
    Duplicate {
        id: String,
        name: String,
    },
    /// Rename and/or change a profile of the user's own.
    Update {
        id: String,
        name: Option<String>,
        settings: Option<ProfileSettings>,
    },
    /// Delete a profile of the user's own; if it was active, "Default" is.
    Delete {
        id: String,
    },
}

/// One profile, for the UI. Built-in profiles have no name here: the UI words them in the user's language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDto {
    pub id: String,
    pub name: Option<String>,
    pub built_in: bool,
    pub settings: ProfileSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesDto {
    pub active: String,
    pub profiles: Vec<ProfileDto>,
    /// The user's own settings (what "Default" uses, and what a profile leaves alone).
    pub base: EffectiveSettings,
    /// What is in force now.
    pub effective: EffectiveSettings,
}

impl ProfilesDto {
    pub fn new(saved: &Saved, base: EffectiveSettings) -> Self {
        let mut profiles: Vec<ProfileDto> = BUILT_IN
            .iter()
            .map(|id| ProfileDto { id: (*id).into(), name: None, built_in: true, settings: built_in(id).unwrap_or_default() })
            .collect();
        profiles.extend(saved.custom.iter().map(|c| ProfileDto {
            id: c.id.clone(),
            name: Some(c.name.clone()),
            built_in: false,
            settings: c.settings,
        }));
        Self { active: saved.active.clone(), profiles, base, effective: resolve(base, saved.active_settings()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: EffectiveSettings =
        EffectiveSettings { speed_limit_bps: Some(500_000), max_active: 3, max_connections: 16, smart_on: false, adaptive: false };

    #[test]
    fn default_is_exactly_the_users_settings() {
        assert_eq!(resolve(BASE, built_in("default").unwrap()), BASE);
    }

    #[test]
    fn built_in_profiles_only_change_what_they_name() {
        let fast = resolve(BASE, built_in("fast").unwrap());
        assert_eq!((fast.max_connections, fast.adaptive), (FAST_CONNECTIONS, false));
        assert_eq!((fast.speed_limit_bps, fast.max_active), (None, BASE.max_active), "full speed: no limit; the user's slots stay");
        let balanced = resolve(BASE, built_in("balanced").unwrap());
        assert!(balanced.smart_on && balanced.adaptive, "Smart throttle's automatic mode");
        assert_eq!(balanced.max_connections, BASE.max_connections);
        let low = resolve(BASE, built_in("lowResource").unwrap());
        assert_eq!((low.max_active, low.max_connections), (LOW_ACTIVE, LOW_CONNECTIONS));
        assert_eq!(low.speed_limit_bps, BASE.speed_limit_bps);
        // Fast holds Smart throttle off entirely (full speed, never giving way), whatever the user's switches say.
        let on = EffectiveSettings { smart_on: true, adaptive: true, ..BASE };
        assert_eq!((resolve(on, built_in("fast").unwrap()).smart_on, resolve(on, built_in("fast").unwrap()).adaptive), (false, false));
        assert!(built_in("nope").is_none());
    }

    #[test]
    fn a_custom_speed_limit_can_set_or_lift_the_limit() {
        let set = ProfileSettings { speed_limit_bps: Some(100_000), ..Default::default() };
        assert_eq!(resolve(BASE, set).speed_limit_bps, Some(100_000));
        let lift = ProfileSettings { speed_limit_bps: Some(0), ..Default::default() };
        assert_eq!(resolve(BASE, lift).speed_limit_bps, None);
    }

    #[test]
    fn invalid_values_are_refused() {
        let bad = [
            ProfileSettings { max_active: Some(0), ..Default::default() },
            ProfileSettings { max_active: Some(33), ..Default::default() },
            ProfileSettings { max_connections: Some(0), ..Default::default() },
            ProfileSettings { max_connections: Some(MAX_CONNECTIONS + 1), ..Default::default() },
            ProfileSettings { speed_limit_bps: Some(1), ..Default::default() },
        ];
        for b in bad {
            assert!(validate_settings(&b).is_err(), "{b:?}");
        }
        assert!(validate_settings(&ProfileSettings {
            speed_limit_bps: Some(0),
            max_active: Some(32),
            max_connections: Some(64),
            adaptive: Some(true)
        })
        .is_ok());
        for n in ["", "   ", "a\u{7}b", &"x".repeat(41)] {
            assert!(validate_name(n).is_err(), "{n:?}");
        }
        assert_eq!(validate_name("  Night  ").unwrap(), "Night");
        // Unknown fields are refused by the parser too.
        assert!(serde_json::from_str::<ProfileSettings>(r#"{"maxActive":2,"bypassTls":true}"#).is_err());
    }

    #[test]
    fn the_lifecycle_keeps_built_ins_and_falls_back_to_default() {
        let mut s = Saved::default();
        let night = ProfileSettings { speed_limit_bps: Some(0), max_active: Some(5), ..Default::default() };
        s.apply(ProfileAction::Create { name: "Night".into(), settings: night }).unwrap();
        assert!(s.apply(ProfileAction::Create { name: "night".into(), settings: night }).is_err(), "names are unique");
        s.apply(ProfileAction::Duplicate { id: "fast".into(), name: "Fast copy".into() }).unwrap();
        assert_eq!(s.custom[1].settings, built_in("fast").unwrap());
        s.apply(ProfileAction::Select { id: "c1".into() }).unwrap();
        s.apply(ProfileAction::Update { id: "c1".into(), name: Some("Nights".into()), settings: None }).unwrap();
        assert_eq!((s.custom[0].name.as_str(), s.custom[0].settings), ("Nights", night));
        assert!(s
            .apply(ProfileAction::Update {
                id: "c1".into(),
                name: None,
                settings: Some(ProfileSettings { max_active: Some(99), ..Default::default() })
            })
            .is_err());
        assert_eq!(s.custom[0].settings, night, "an invalid edit changes nothing");
        // Built-ins are protected.
        for id in BUILT_IN {
            assert!(s.apply(ProfileAction::Delete { id: id.into() }).is_err());
            assert!(s.apply(ProfileAction::Update { id: id.into(), name: Some("x".into()), settings: None }).is_err());
        }
        // Deleting the active profile: back to Default (the user's own settings).
        s.apply(ProfileAction::Delete { id: "c1".into() }).unwrap();
        assert_eq!(s.active, DEFAULT_ID);
        assert!(s.apply(ProfileAction::Select { id: "c1".into() }).is_err(), "gone");
        // Ids are never reused.
        s.apply(ProfileAction::Create { name: "Again".into(), settings: night }).unwrap();
        assert_eq!(s.custom.last().unwrap().id, "c3");
        for i in s.custom.len()..MAX_CUSTOM {
            s.apply(ProfileAction::Create { name: format!("p{i}"), settings: night }).unwrap();
        }
        assert!(s.apply(ProfileAction::Create { name: "one too many".into(), settings: night }).is_err());
    }

    #[test]
    fn saved_data_that_is_missing_corrupt_or_odd_is_recovered() {
        assert_eq!(Saved::parse(None), (Saved::default(), false));
        assert_eq!(Saved::parse(Some("{not json")), (Saved::default(), true));
        assert_eq!(Saved::parse(Some(r#"{"version":2,"active":"fast","custom":[],"nextId":1}"#)).0, Saved::default(), "a newer version");
        // An invalid custom profile is dropped; an unknown active one falls back to Default.
        let text = r#"{"version":1,"active":"c9","nextId":1,"custom":[
            {"id":"c1","name":"Ok","settings":{"maxActive":2}},
            {"id":"c2","name":"Bad","settings":{"maxActive":500}},
            {"id":"c1","name":"Twin","settings":{}},
            {"id":"zz","name":"Odd id","settings":{}}]}"#;
        let (s, repaired) = Saved::parse(Some(text));
        assert!(repaired);
        assert_eq!(s.custom.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c1"]);
        assert_eq!(s.active, DEFAULT_ID);
        assert_eq!(s.next_id, 2, "never reuses an id that exists");
        let good = serde_json::to_string(&s).unwrap();
        assert_eq!(Saved::parse(Some(&good)), (s, false), "round trip");
    }

    #[test]
    fn the_dto_lists_built_ins_first_and_shows_what_is_in_force() {
        let mut s = Saved::default();
        s.apply(ProfileAction::Select { id: "lowResource".into() }).unwrap();
        let d = ProfilesDto::new(&s, BASE);
        assert_eq!(d.profiles.iter().take(4).map(|p| p.id.as_str()).collect::<Vec<_>>(), BUILT_IN);
        assert_eq!((d.base, d.effective.max_active), (BASE, LOW_ACTIVE));
    }
}
