use crate::cameras::CameraRecordingMode;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(super) struct CameraRecordingPolicy {
    mode: CameraRecordingMode,
    event_duration: Duration,
    main_until: Option<Instant>,
    event_main_state: EventMainState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EventMainState {
    #[default]
    Idle,
    WaitingForKeyframe,
    Recording,
}

pub(super) enum AdmissionDecision {
    Record,
    RecordAs(&'static str),
    Ignore,
}

impl CameraRecordingPolicy {
    pub(super) const fn new(mode: CameraRecordingMode, event_duration: Duration) -> Self {
        Self {
            mode,
            event_duration,
            main_until: None,
            event_main_state: EventMainState::Idle,
        }
    }

    pub(super) fn note_event(&mut self, now: Instant) {
        if self.mode != CameraRecordingMode::EventBoost {
            return;
        }
        self.main_until = now.checked_add(self.event_duration);
        if self.event_main_state == EventMainState::Idle {
            self.event_main_state = EventMainState::WaitingForKeyframe;
        }
    }

    pub(super) fn decide(
        &mut self,
        stream_id: &str,
        is_video: bool,
        is_video_keyframe: bool,
        now: Instant,
    ) -> AdmissionDecision {
        match (self.mode, stream_id) {
            (CameraRecordingMode::Sub, "sub")
            | (CameraRecordingMode::Main, "main")
            | (CameraRecordingMode::Both, "main" | "sub") => AdmissionDecision::Record,
            (CameraRecordingMode::EventBoost, "main" | "sub") if !is_video => {
                let preferred = if self.event_main_state == EventMainState::Recording {
                    "main"
                } else {
                    "sub"
                };
                if stream_id == preferred {
                    AdmissionDecision::RecordAs("sub")
                } else {
                    AdmissionDecision::Ignore
                }
            }
            (CameraRecordingMode::EventBoost, "main" | "sub") => {
                if self.event_main_state == EventMainState::WaitingForKeyframe
                    && self.main_until.is_none_or(|deadline| now >= deadline)
                {
                    self.event_main_state = EventMainState::Idle;
                    self.main_until = None;
                }
                match self.event_main_state {
                    EventMainState::Idle if stream_id == "sub" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::WaitingForKeyframe if stream_id == "sub" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::WaitingForKeyframe
                        if stream_id == "main" && is_video_keyframe =>
                    {
                        self.event_main_state = EventMainState::Recording;
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording
                        if self.main_until.is_some_and(|deadline| now < deadline)
                            && stream_id == "main" =>
                    {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording
                        if stream_id == "sub"
                            && is_video_keyframe
                            && self.main_until.is_none_or(|deadline| now >= deadline) =>
                    {
                        self.event_main_state = EventMainState::Idle;
                        self.main_until = None;
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording if stream_id == "main" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    _ => AdmissionDecision::Ignore,
                }
            }
            _ => AdmissionDecision::Ignore,
        }
    }

    pub(super) const fn preferred_audio_stream(&self) -> &'static str {
        match self.mode {
            CameraRecordingMode::Main | CameraRecordingMode::Both => "main",
            CameraRecordingMode::EventBoost
                if matches!(self.event_main_state, EventMainState::Recording) =>
            {
                "main"
            }
            _ => "sub",
        }
    }
}
