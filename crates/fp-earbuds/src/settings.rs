//! The user's earbud settings. Four values; none identifies a device.
//!
//! Loading is forgiving: a missing key takes its default and an unknown value of
//! `pause_rule` (a setting from a newer build, say) falls back to the default instead of
//! failing the whole settings file.

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::state::Wear;

/// When an earbud coming out pauses the video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseRule {
    /// Never.
    Off,
    /// As soon as one earbud is out.
    #[default]
    EitherBud,
    /// Only when both are out.
    BothBuds,
}

impl PauseRule {
    /// Whether this wear state is one the rule pauses in.
    pub fn pauses_on(self, wear: Wear) -> bool {
        match self {
            PauseRule::Off => false,
            PauseRule::EitherBud => matches!(wear, Wear::Partial | Wear::NotWorn),
            PauseRule::BothBuds => wear == Wear::NotWorn,
        }
    }
}

/// Earbuds (AirPods and Beats) through a local MagicPodsCore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EarbudsSettings {
    /// Off by default: no thread and no socket until turned on.
    pub enabled: bool,
    /// When an earbud coming out pauses the video.
    #[serde(deserialize_with = "lenient")]
    pub pause_rule: PauseRule,
    /// Resume when the earbuds are back in; only a pause the app made itself.
    pub resume_on_reinsert: bool,
    /// Show the battery on the control bar.
    pub show_battery_in_hud: bool,
}

impl Default for EarbudsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            pause_rule: PauseRule::default(),
            resume_on_reinsert: true,
            show_battery_in_hud: true,
        }
    }
}

/// Reads any value and falls back to the type's default when it does not fit.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let s = EarbudsSettings::default();
        assert!(!s.enabled);
        assert_eq!(s.pause_rule, PauseRule::EitherBud);
        assert!(s.resume_on_reinsert);
        assert!(s.show_battery_in_hud);
    }

    #[test]
    fn partial_files_take_defaults() {
        let s: EarbudsSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(s.enabled);
        assert_eq!(s.pause_rule, PauseRule::EitherBud);
        assert!(s.resume_on_reinsert);
        let s: EarbudsSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, EarbudsSettings::default());
    }

    #[test]
    fn an_unknown_rule_falls_back_and_keeps_the_rest() {
        let text = r#"{"pause_rule":"left_only","resume_on_reinsert":false}"#;
        let s: EarbudsSettings = serde_json::from_str(text).unwrap();
        assert_eq!(s.pause_rule, PauseRule::EitherBud);
        assert!(!s.resume_on_reinsert);
        let s: EarbudsSettings = serde_json::from_str(r#"{"pause_rule":7}"#).unwrap();
        assert_eq!(s.pause_rule, PauseRule::EitherBud);
    }

    #[test]
    fn round_trip_with_frozen_names() {
        let s = EarbudsSettings {
            enabled: true,
            pause_rule: PauseRule::BothBuds,
            resume_on_reinsert: false,
            show_battery_in_hud: false,
        };
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains(r#""pause_rule":"both_buds""#), "{text}");
        let back: EarbudsSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
        let off = serde_json::to_string(&PauseRule::Off).unwrap();
        assert_eq!(off, r#""off""#);
        let either = serde_json::to_string(&PauseRule::EitherBud).unwrap();
        assert_eq!(either, r#""either_bud""#);
    }

    #[test]
    fn what_each_rule_pauses_on() {
        let states = [Wear::Worn, Wear::Partial, Wear::NotWorn];
        let row = |rule: PauseRule| states.map(|w| rule.pauses_on(w));
        assert_eq!(row(PauseRule::Off), [false, false, false]);
        assert_eq!(row(PauseRule::EitherBud), [false, true, true]);
        assert_eq!(row(PauseRule::BothBuds), [false, false, true]);
        for rule in [PauseRule::EitherBud, PauseRule::BothBuds] {
            assert!(!rule.pauses_on(Wear::Unknown));
            assert!(!rule.pauses_on(Wear::NotAvailable));
        }
    }
}
