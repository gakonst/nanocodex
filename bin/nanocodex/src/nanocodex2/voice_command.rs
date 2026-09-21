//! Local voice controls. Arguments never enter a model prompt.
use std::path::PathBuf;

pub(crate) const HELP: &str = concat!(
    "Voice controls\n\n",
    "/voice [menu] — open the voice menu\n",
    "/voice on|off|mute|unmute|status\n\n",
    "Choose a provider: /voice voices\nList voices: /voice voices chatgpt|elevenlabs\n",
    "Switch: /voice chatgpt NAME or /voice elevenlabs VOICE_ID\n",
    "Legacy ChatGPT shortcut: /voice NAME\n\n",
    "Record a clone: /voice clone [\"NAME\"] opens a local recording panel.\n",
    "/voice clone record · stop · review · submit --consent · cancel\n",
    "File clone: /voice clone \"NAME\" \"AUDIO_PATH\" --consent\n",
    "--consent confirms you own the voice or have permission to clone it.\n",
    "Audio uploads directly to ElevenLabs using local ELEVENLABS_API_KEY; it never goes to chat.\n",
    "Quote names and paths containing spaces. Cloning does not switch the active voice.\n\n",
    "Switching an active voice reconnects safely and preserves microphone mute.\n",
    "/voice help — show this panel"
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Provider {
    Chatgpt,
    ElevenLabs,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Selection {
    Chatgpt(&'static str),
    ElevenLabs(String),
}
impl Default for Selection {
    fn default() -> Self {
        Self::Chatgpt("cove")
    }
}
impl Selection {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Chatgpt(name) => format!("ChatGPT {name}"),
            Self::ElevenLabs(id) => format!("ElevenLabs {id}"),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Toggle,
    Start(Option<&'static str>),
    Select(Selection),
    List,
    ListProvider(Provider),
    Clone { name: String, path: PathBuf },
    CloneOpen(String),
    CloneRecord(Option<String>),
    CloneStop,
    CloneSubmit,
    CloneCancel,
    CloneReview,
    ClonePlay,
    Stop,
    ToggleMute,
    Unmute,
    Status,
    Help,
}
impl Command {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        let words = words(value)?;
        let args: Vec<&str> = words.iter().map(String::as_str).collect();
        let voice = |name| {
            nanocodex_voice_protocol::CHATGPT_REALTIME_VOICES
                .iter()
                .find(|voice| **voice == name)
                .copied()
                .ok_or_else(|| format!("Unknown ChatGPT voice. Use /voice voices chatgpt. {HELP}"))
        };
        match args.as_slice() {
            [] | ["menu"] => Ok(Self::Toggle),
            ["start" | "on"] => Ok(Self::Start(None)),
            ["voices"] => Ok(Self::List),
            ["voices", "chatgpt"] => Ok(Self::ListProvider(Provider::Chatgpt)),
            ["voices", "elevenlabs"] => Ok(Self::ListProvider(Provider::ElevenLabs)),
            ["chatgpt", name] => Ok(Self::Select(Selection::Chatgpt(voice(*name)?))),
            ["elevenlabs", id] if !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => Ok(Self::Select(Selection::ElevenLabs((*id).into()))),
            ["clone"] => Ok(Self::CloneOpen("My voice".into())),
            ["clone", "stop"] => Ok(Self::CloneStop),
            ["clone", "cancel"] => Ok(Self::CloneCancel),
            ["clone", "play"] => Ok(Self::ClonePlay),
            ["clone", "review"] => Ok(Self::CloneReview),
            ["clone", "submit", "--consent"] => Ok(Self::CloneSubmit),
            ["clone", "submit"] => Err("Upload requires /voice clone submit --consent. This confirms you own the voice or have permission to clone it.".into()),
            ["clone", "record"] => Ok(Self::CloneRecord(None)),
            ["clone", "record", name] if !name.trim().is_empty() => Ok(Self::CloneRecord(Some((*name).into()))),
            ["clone", name] if !name.trim().is_empty() => Ok(Self::CloneOpen((*name).into())),
            ["clone", name, path, "--consent"] if !name.trim().is_empty() && !path.is_empty() => Ok(Self::Clone { name: (*name).into(), path: (*path).into() }),
            ["clone", ..] => Err("Usage: /voice clone \"NAME\" \"AUDIO_PATH\" --consent. --consent confirms you own the voice or have permission to clone it; audio is uploaded directly to ElevenLabs.".into()),
            ["stop" | "off"] => Ok(Self::Stop),
            ["mute"] => Ok(Self::ToggleMute),
            ["unmute"] => Ok(Self::Unmute),
            ["status"] => Ok(Self::Status),
            ["help"] => Ok(Self::Help),
            [name] => Ok(Self::Start(Some(voice(*name)?))),
            _ => Err(HELP.into()),
        }
    }
}

// Shell-style quoting without expansion, execution, or reading file contents.
fn words(input: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quote != Some('\'') => {
                if quote == Some('"') && !matches!(chars.peek(), Some('\\' | '"')) {
                    word.push('\\');
                } else {
                    word.push(chars.next().ok_or("Trailing escape in /voice arguments")?);
                }
                started = true;
            }
            '\'' | '"' if quote == Some(c) => quote = None,
            '\'' | '"' if quote.is_none() => {
                quote = Some(c);
                started = true;
            }
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    result.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("Unclosed quote in /voice arguments".into());
    }
    if started {
        result.push(word);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_commands_keep_upload_consent_explicit() {
        assert_eq!(
            Command::parse("clone").unwrap(),
            Command::CloneOpen("My voice".into())
        );
        assert_eq!(Command::parse("on").unwrap(), Command::Start(None));
        assert_eq!(Command::parse("off").unwrap(), Command::Stop);
        assert_eq!(
            Command::parse("clone \"Synthetic voice\"").unwrap(),
            Command::CloneOpen("Synthetic voice".into())
        );
        assert_eq!(
            Command::parse("clone record").unwrap(),
            Command::CloneRecord(None)
        );
        assert_eq!(
            Command::parse("clone record \"Synthetic voice\"").unwrap(),
            Command::CloneRecord(Some("Synthetic voice".into()))
        );
        assert_eq!(Command::parse("clone stop").unwrap(), Command::CloneStop);
        assert_eq!(
            Command::parse("clone review").unwrap(),
            Command::CloneReview
        );
        assert_eq!(
            Command::parse("clone cancel").unwrap(),
            Command::CloneCancel
        );
        assert!(Command::parse("clone submit").is_err());
        assert!(Command::parse("clone submit --consent extra").is_err());
        assert_eq!(
            Command::parse("clone submit --consent").unwrap(),
            Command::CloneSubmit
        );
    }
    #[test]
    fn parses_quoted_clone_and_requires_consent() {
        assert_eq!(
            Command::parse("clone \"My voice\" '/tmp/my sample.wav' --consent").unwrap(),
            Command::Clone {
                name: "My voice".into(),
                path: "/tmp/my sample.wav".into()
            }
        );
        assert!(Command::parse("clone me sample.wav").is_err());
        assert!(Command::parse("clone 'unclosed").is_err());
        assert!(Command::parse("clone '' x --consent").is_err());
    }
    #[test]
    fn parses_providers_and_legacy_controls() {
        assert_eq!(Command::parse("menu").unwrap(), Command::parse("").unwrap());
        assert_eq!(
            Command::parse("chatgpt cove").unwrap(),
            Command::Select(Selection::Chatgpt("cove"))
        );
        assert_eq!(
            Command::parse("elevenlabs voice_123").unwrap(),
            Command::Select(Selection::ElevenLabs("voice_123".into()))
        );
        assert_eq!(
            Command::parse("cove").unwrap(),
            Command::Start(Some("cove"))
        );
        assert_eq!(
            Command::parse("voices elevenlabs").unwrap(),
            Command::ListProvider(Provider::ElevenLabs)
        );
        assert!(Command::parse("elevenlabs ../foo").is_err());
        assert!(Command::parse("on extra").is_err());
    }
}
