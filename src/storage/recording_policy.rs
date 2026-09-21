use super::recording_control::{Clock, Control};
use crate::cameras::CameraRecordingMode;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub(super) struct CameraRecordingPolicy {
    pub(super) control: Control,
    event_duration: Duration,
    main_until: Option<Instant>,
    event_main_state: EventMainState,
    awaiting_keyframe: [bool; 2],
    discontinuity: [bool; 2],
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
    pub(super) fn new(mode: CameraRecordingMode, event_duration: Duration) -> Self {
        Self {
            control: Control::new(mode),
            event_duration,
            main_until: None,
            event_main_state: EventMainState::Idle,
            awaiting_keyframe: [false; 2],
            discontinuity: [false; 2],
        }
    }

    pub(super) fn note_event(&mut self, clock: Clock) {
        if !self.sync_permission(clock)
            || self.control.configured_mode() != CameraRecordingMode::EventBoost
        {
            return;
        }
        self.main_until = clock.monotonic.checked_add(self.event_duration);
        if self.event_main_state == EventMainState::Idle {
            self.event_main_state = EventMainState::WaitingForKeyframe;
        }
    }

    pub(super) fn decide(
        &mut self,
        stream_id: &str,
        is_video: bool,
        is_video_keyframe: bool,
        clock: Clock,
    ) -> AdmissionDecision {
        if !self.sync_permission(clock) {
            return AdmissionDecision::Ignore;
        }
        let Some(index) = stream_index(stream_id) else {
            return AdmissionDecision::Ignore;
        };
        if self.awaiting_keyframe[index] {
            if !is_video || !is_video_keyframe {
                return AdmissionDecision::Ignore;
            }
            self.awaiting_keyframe[index] = false;
        }
        self.select(stream_id, is_video, is_video_keyframe, clock.monotonic)
    }

    fn select(
        &mut self,
        stream_id: &str,
        is_video: bool,
        is_video_keyframe: bool,
        now: Instant,
    ) -> AdmissionDecision {
        match (self.control.configured_mode(), stream_id) {
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
        match self.control.configured_mode() {
            CameraRecordingMode::Main | CameraRecordingMode::Both => "main",
            CameraRecordingMode::EventBoost
                if matches!(self.event_main_state, EventMainState::Recording) =>
            {
                "main"
            }
            _ => "sub",
        }
    }

    pub(super) fn reconfigure(&mut self, mode: CameraRecordingMode, duration: Duration) {
        if self.control.configured_mode() == mode && self.event_duration == duration {
            return;
        }
        self.control.configure(mode);
        self.event_duration = duration;
        self.reset_coverage();
    }

    pub(super) fn sync_permission(&mut self, now: Clock) -> bool {
        let permitted = self.control.effective(now).0 != CameraRecordingMode::Off;
        if !permitted {
            self.reset_coverage();
        }
        permitted
    }

    fn reset_coverage(&mut self) {
        self.main_until = None;
        self.event_main_state = EventMainState::Idle;
        self.awaiting_keyframe = [true; 2];
        self.discontinuity = [true; 2];
    }

    pub(super) fn take_discontinuity(&mut self, stream: &str) -> bool {
        stream_index(stream).is_some_and(|index| std::mem::take(&mut self.discontinuity[index]))
    }
}

fn stream_index(stream: &str) -> Option<usize> {
    match stream {
        "main" => Some(0),
        "sub" => Some(1),
        _ => None,
    }
}
