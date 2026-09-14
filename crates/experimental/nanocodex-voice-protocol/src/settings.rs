//! User-facing voice policy, shared by native, WASM, and Rust consumers.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VoicePace {
    Slow,
    #[default]
    Natural,
    Fast,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceUpdates {
    #[default]
    Auto,
    Results,
    Silent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceHandoffMode {
    #[default]
    Thinking,
    Commentary,
    BemTags,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct VoiceSettings {
    pub voice: String,
    /// Additional speaking preferences; the base assistant instructions are retained.
    pub instructions: String,
    pub pace: VoicePace,
    pub updates: VoiceUpdates,
    pub handoff_mode: VoiceHandoffMode,
    /// `None` retains the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgements: Option<bool>,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            voice: crate::CHATGPT_REALTIME_VOICE.to_owned(),
            instructions: String::new(),
            pace: VoicePace::Natural,
            updates: VoiceUpdates::Auto,
            handoff_mode: VoiceHandoffMode::Thinking,
            acknowledgements: None,
        }
    }
}

impl VoiceSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.instructions.contains('\0') {
            return Err("voice instructions must not contain NUL".to_owned());
        }
        if self.voice.is_empty() || self.voice.len() > 64 {
            return Err("invalid voice name".to_owned());
        }
        Ok(())
    }

    pub fn validate_chatgpt(&self) -> Result<(), String> {
        self.validate()?;
        if !crate::CHATGPT_REALTIME_VOICES.contains(&self.voice.as_str()) {
            return Err(format!("unsupported ChatGPT voice: {}", self.voice));
        }
        Ok(())
    }

    #[must_use]
    pub fn instructions(&self, base: &str) -> String {
        let mut result = base.to_owned();
        match self.pace {
            VoicePace::Natural => {}
            VoicePace::Slow => {
                result.push_str("\nSpeak at a relaxed, unhurried pace with clear pauses.")
            }
            VoicePace::Fast => result.push_str("\nSpeak briskly and clearly. Keep pauses short."),
        }
        match self.updates {
            VoiceUpdates::Auto => {}
            VoiceUpdates::Results => result.push_str("\nDo not narrate intermediate work. Speak when a result, blocker, or question needs the user's attention."),
            VoiceUpdates::Silent => result.push_str("\nDo not speak unsolicited background progress or results. Answer when the user addresses you or explicitly requests a spoken update."),
        }
        if !self.instructions.trim().is_empty() {
            result.push_str("\n\n## User speaking preferences\n");
            result.push_str(self.instructions.trim());
        }
        result
    }

    pub fn chatgpt_session(&self, base: &str) -> Result<Value, String> {
        self.validate_chatgpt()?;
        let mut session = json!({
            "model": crate::CHATGPT_REALTIME_MODEL,
            "instructions": self.instructions(base),
            "audio": { "output": { "voice": self.voice } },
            "delegation": { "type": "client" },
        });
        if let Some(enabled) = self.acknowledgements {
            session["delegation"]["ack_filler"] = json!(enabled);
        }
        Ok(session)
    }

    #[must_use]
    pub fn effective_handoff_mode(&self) -> VoiceHandoffMode {
        match self.updates {
            VoiceUpdates::Auto => self.handoff_mode,
            VoiceUpdates::Results => VoiceHandoffMode::BemTags,
            VoiceUpdates::Silent => VoiceHandoffMode::Commentary,
        }
    }

    pub(crate) fn output_channel(&self, phase: Option<&str>) -> Option<&'static str> {
        match self.effective_handoff_mode() {
            VoiceHandoffMode::Thinking => None,
            VoiceHandoffMode::Commentary => Some("commentary"),
            VoiceHandoffMode::BemTags => match phase {
                Some("commentary") => Some("commentary"),
                _ => Some("speakable"),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTextRole {
    #[default]
    User,
    Developer,
    Assistant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_session_preserves_defaults_and_appends_preferences() {
        let defaults = VoiceSettings::default()
            .chatgpt_session("Base instructions")
            .unwrap();
        assert_eq!(
            defaults,
            json!({
                "model": "gpt-live-1-codex", "instructions": "Base instructions",
                "audio": { "output": { "voice": "cove" } }, "delegation": { "type": "client" },
            })
        );
        let settings: VoiceSettings = serde_json::from_value(json!({
            "voice": "maple", "pace": "slow", "updates": "results",
            "instructions": "Speak Greek.", "acknowledgements": false,
        }))
        .unwrap();
        let session = settings
            .chatgpt_session("Keep delegation instructions.")
            .unwrap();
        assert_eq!(session["audio"]["output"]["voice"], "maple");
        assert_eq!(session["delegation"]["ack_filler"], false);
        let instructions = session["instructions"].as_str().unwrap();
        assert!(instructions.starts_with("Keep delegation instructions."));
        assert!(instructions.contains("unhurried"));
        assert!(instructions.ends_with("Speak Greek."));
        assert_eq!(
            settings.output_channel(Some("commentary")),
            Some("commentary")
        );
        assert_eq!(
            settings.output_channel(Some("final_answer")),
            Some("speakable")
        );
    }

    #[test]
    fn settings_reject_custom_voices_platform_options_and_invalid_preferences() {
        for value in [
            json!({"turnDetection": "semantic_vad"}),
            json!({"speed": 1.5}),
            json!({"acknowledgements": "false"}),
        ] {
            assert!(serde_json::from_value::<VoiceSettings>(value).is_err());
        }
        for voice in ["alloy", "voice_custom", ""] {
            assert!(
                VoiceSettings {
                    voice: voice.to_owned(),
                    ..Default::default()
                }
                .validate_chatgpt()
                .is_err()
            );
        }
        assert!(
            VoiceSettings {
                instructions: "\0".to_owned(),
                ..Default::default()
            }
            .validate_chatgpt()
            .is_err()
        );
        let instructions = "🦊".repeat(2049);
        let session = VoiceSettings {
            instructions: instructions.clone(),
            ..Default::default()
        }
        .chatgpt_session("Base")
        .unwrap();
        assert!(
            session["instructions"]
                .as_str()
                .unwrap()
                .ends_with(&instructions)
        );
    }
}
