//! Download Profiles, manager side: keeps the saved profiles and applies the active one's values to the existing
//! controls. The rules (presets, validation, precedence) are in `crate::profiles`. The user's own settings are never
//! written by a profile; [`DownloadManager::apply_profile`] is the one place that puts the values in force:
//!
//! * the speed limiter's global layer (`RateLimits::set_global`; per-download, per-type and Smart throttle layers
//!   are untouched, the lowest still wins);
//! * the queue's run slots (`SlotScheduler::set_max_slots`: only downloads waiting for a slot may start; a paused
//!   download is not waiting and stays paused; running ones are never stopped, a lower count applies as they end);
//! * the engine's connection ceiling (`Downloader::set_max_connections`: running downloads follow it at their next
//!   sample, fewer connections give way without a restart);
//! * Smart throttle's automatic mode (`SmartState::profile`).

use crate::profiles::{self, EffectiveSettings, ProfileAction, ProfilesDto, Saved};

use super::*;

const PROFILES_KEY: &str = "profiles";

#[derive(Default)]
pub(super) struct ProfilesState {
    saved: Saved,
    /// The user's own global speed limit (the limiter holds the value in force).
    base_global: Option<u64>,
    /// What was last put in force (nothing is re-applied when unchanged).
    applied: Option<(EffectiveSettings, Option<bool>)>,
}

impl ProfilesState {
    /// The saved profiles (or the defaults: a missing, unreadable or newer value never stops the app or changes the
    /// user's settings) and the user's global limit.
    pub(super) fn load(store: &SqliteStore, base_global: Option<u64>) -> Self {
        let text = store.get_setting(PROFILES_KEY).ok().flatten();
        let (saved, repaired) = Saved::parse(text.as_deref());
        if repaired {
            tracing::warn!(active = %saved.active, kept = saved.custom.len(), "saved download profiles were invalid in part; using what is valid");
        }
        Self { saved, base_global, applied: None }
    }
}

impl DownloadManager {
    /// The user's own settings (what "Default" is, and what a profile leaves alone).
    pub(super) fn base_settings(&self) -> EffectiveSettings {
        let speed_limit_bps = self.inner.profiles.lock().unwrap().base_global;
        let (max_active, max_connections) = {
            let q = self.inner.queue.lock().unwrap();
            (q.max_active() as u32, q.max_connections() as u32)
        };
        let (smart_on, adaptive) = {
            let s = self.inner.smart.lock().unwrap();
            (s.user_enabled(), s.user_auto())
        };
        EffectiveSettings { speed_limit_bps, max_active, max_connections, smart_on, adaptive }
    }

    /// Note a new global limit of the user's own, then put the values in force again.
    pub(super) fn set_base_global(&self, limit: Option<u64>) {
        self.inner.profiles.lock().unwrap().base_global = limit;
        self.apply_profile();
    }

    /// Put the active profile's values (over the user's settings) in force. Cheap, and nothing happens when nothing
    /// changed; never starts, resumes, pauses or restarts a download by itself.
    pub(super) fn apply_profile(&self) {
        let base = self.base_settings();
        let (eff, adaptive) = {
            let p = self.inner.profiles.lock().unwrap();
            let active = p.saved.active_settings();
            (profiles::resolve(base, active), active.adaptive)
        };
        {
            let mut p = self.inner.profiles.lock().unwrap();
            if p.applied == Some((eff, adaptive)) {
                return;
            }
            p.applied = Some((eff, adaptive));
        }
        self.inner.limits.set_global(eff.speed_limit_bps);
        self.inner.scheduler.set_max_slots(eff.max_active as usize);
        self.inner.downloader.set_max_connections(eff.max_connections as usize);
        let changed = self.inner.smart.lock().unwrap().set_profile(adaptive);
        if changed {
            self.smart_profile_changed();
        }
        tracing::info!(
            limit = ?eff.speed_limit_bps,
            active = eff.max_active,
            connections = eff.max_connections,
            adaptive = eff.adaptive,
            "download settings in force"
        );
    }

    pub fn get_profiles(&self) -> ProfilesDto {
        let base = self.base_settings();
        ProfilesDto::new(&self.inner.profiles.lock().unwrap().saved, base)
    }

    /// Select, create, duplicate, change or delete a profile. Checked first, saved, and only then in force: an
    /// invalid request changes nothing.
    pub async fn profile_action(&self, action: ProfileAction) -> Result<ProfilesDto, ManagerError> {
        let mut next = self.inner.profiles.lock().unwrap().saved.clone();
        next.apply(action)?;
        let text = serde_json::to_string(&next).map_err(|e| ManagerError::Internal(e.to_string()))?;
        let store = self.inner.store.clone();
        blocking(move || store.set_setting(PROFILES_KEY, &text)).await?;
        self.inner.profiles.lock().unwrap().saved = next;
        self.apply_profile();
        let dto = self.get_profiles();
        self.emit(ManagerEvent::Profiles { profiles: dto.clone() });
        self.emit_queue();
        Ok(dto)
    }
}
