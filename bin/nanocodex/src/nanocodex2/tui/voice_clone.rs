//! Local-only recording ownership. Dropping a task or state drops its audio resources.
use crate::voice_recording::{RecordedSample, Recorder};
use tokio::task::JoinSet;

pub(super) enum State {
    Ready,
    Waiting,
    Busy,
    Starting,
    Stopping,
    Playing,
    Recording(Recorder),
    Review(RecordedSample),
    PlaybackFailed(RecordedSample, String),
}
pub(super) struct Panel {
    pub name: String,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub state: State,
    pub tasks: JoinSet<Result<State, String>>,
}
impl Panel {
    pub fn new(name: String) -> Self {
        Self {
            name,
            error: None,
            notice: None,
            state: State::Ready,
            tasks: JoinSet::new(),
        }
    }
    pub fn text(&self) -> String {
        let detail = match &self.state {
            State::Ready => "Ready · R starts the microphone",
            State::Waiting => "Waiting for realtime voice cleanup · /voice clone cancel",
            State::Busy => "Uploading recording…",
            State::Starting => "Opening your microphone… · Esc cancels",
            State::Stopping => "Saving your recording… · Esc cancels",
            State::Playing => "Playing your recording locally… · Esc cancels",
            State::Recording(_) => "Recording locally · S or Space stops",
            State::Review(_) | State::PlaybackFailed(_, _) => {
                "Recording stopped · P to listen; R to re-record"
            }
        };
        let elapsed = match &self.state {
            State::Recording(recorder) => {
                let seconds = recorder.elapsed().as_secs();
                let peak = recorder.peak();
                let bars = if peak < 128 {
                    0
                } else {
                    (usize::from(peak.min(8192)) * 12 / 8192).clamp(1, 12)
                };
                format!(
                    "\n● RECORDING  {:02}:{:02} / 02:00   mic [{}{}]",
                    seconds / 60,
                    seconds % 60,
                    "▮".repeat(bars),
                    "·".repeat(12 - bars)
                )
            }
            _ => String::new(),
        };
        let duration = match &self.state {
            State::Review(sample) | State::PlaybackFailed(sample, _) => {
                let seconds = sample.duration().as_secs_f64();
                let advice = if seconds < 60.0 {
                    " · Short sample: aim for 60–90s; R re-records"
                } else {
                    ""
                };
                format!("\nRecorded audio: {seconds:.1}s{advice}")
            }
            _ => String::new(),
        };
        let guide = if matches!(self.state, State::Ready | State::Recording(_)) {
            "\nAim for 60–90s (ElevenLabs: 1–2min; recorder cap: 120s).\nUse your natural desired style, steady tone and volume.\nOne speaker; quiet room, no music or reverb. H: read-aloud script\n"
        } else {
            ""
        };
        let notice = self.notice.as_deref().unwrap_or("");
        let error = self.error.as_deref().unwrap_or("");
        format!(
            "Voice clone: {}\n{detail}{elapsed}{duration}\n{notice}{guide}\nR: record/re-record   Space/S: stop   P: play locally\nU: consent + upload to ElevenLabs   Esc: cancel and delete\nPressing U confirms you own this voice or have permission to clone it.\nAudio is recorded locally and never sent to chat. No automatic upload.\n{error}",
            self.name
        )
    }
    pub fn microphone_peak(&self) -> u16 {
        match &self.state {
            State::Recording(recorder) => recorder.peak(),
            _ => 0,
        }
    }
    pub fn start_if_ready(&mut self) {
        if matches!(self.state, State::Waiting) {
            self.state = State::Starting;
            self.tasks
                .spawn(async { Recorder::start().await.map(State::Recording) });
        }
    }
    pub fn record(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            State::Ready | State::Review(_) | State::PlaybackFailed(_, _)
        ) {
            return Err("Stop the current recording or playback before recording again.".into());
        }
        self.error = None;
        self.notice = None;
        self.state = State::Waiting;
        Ok(())
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.stop_with_reason("Stopped by you")
    }
    pub fn stop_with_reason(&mut self, reason: &str) -> Result<(), String> {
        if !matches!(self.state, State::Recording(_)) {
            return Err("No microphone recording is running.".into());
        }
        self.notice = Some(reason.to_owned());
        let State::Recording(recorder) = std::mem::replace(&mut self.state, State::Stopping) else {
            unreachable!()
        };
        self.tasks
            .spawn(async { recorder.stop().await.map(State::Review) });
        Ok(())
    }
    pub fn play(&mut self) -> Result<(), String> {
        if !matches!(self.state, State::Review(_)) {
            return Err("Stop recording before playback.".into());
        }
        let State::Review(sample) = std::mem::replace(&mut self.state, State::Playing) else {
            unreachable!()
        };
        self.error = None;
        self.tasks.spawn(async move {
            match sample.play().await {
                Ok(()) => Ok(State::Review(sample)),
                Err(error) => Ok(State::PlaybackFailed(sample, error)),
            }
        });
        Ok(())
    }
    pub fn review(&self) -> Result<String, String> {
        let State::Review(sample) = &self.state else {
            return Err("Stop a recording before reviewing it.".into());
        };
        Ok(format!(
            "Recording ready for review: {}\nListen to this local audio file before submitting. Nothing has been uploaded.\n/voice clone submit --consent uploads directly to ElevenLabs and confirms you own this voice or have permission to clone it.\n/voice clone cancel deletes the recording.",
            sample.path().display()
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn guidance_is_compact_and_script_is_a_full_practice_passage() {
        let mut panel = Panel::new("Synthetic voice".into());
        assert!(panel.text().contains("60–90s"));
        assert!(panel.text().contains("H: read-aloud script"));
        let script = include_str!("voice_clone_script.txt").trim();
        assert!(!panel.text().contains(script));
        assert!((130..=160).contains(&script.split_whitespace().count()));
        panel.state = State::Stopping;
        panel.notice = Some("Stopped by you".into());
        assert!(!panel.text().contains("H: read-aloud script"));
        assert!(panel.text().contains("Stopped by you"));
        assert!(panel.text().contains("Pressing U confirms"));
    }

    #[tokio::test]
    async fn recording_requires_explicit_start_and_review_requires_stopped_sample() {
        let mut panel = Panel::new("Synthetic voice".into());
        assert!(matches!(panel.state, State::Ready));
        assert!(panel.tasks.is_empty());
        assert!(panel.review().is_err());
        assert!(panel.stop().is_err());
        panel.record().unwrap();
        assert!(matches!(panel.state, State::Waiting));
        assert!(panel.record().is_err());
        assert!(panel.tasks.is_empty()); // Caller must confirm realtime cleanup first.
    }
}
