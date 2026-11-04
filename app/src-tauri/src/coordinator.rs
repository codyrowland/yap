use std::time::{Duration, Instant};

use crate::speech::SpeechConfig;

/// A little longer than the audio capture limit (`audio::MAX_SECONDS`), which
/// stops the recording itself; this only guards against a lost release.
const RECORDING_LIMIT: Duration = Duration::from_secs(330);
/// Long recordings take longer to upload or run locally.
const TRANSCRIPTION_LIMIT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub microphone_index: usize,
    pub speech: SpeechConfig,
    /// ISO-639-1 code, or `None` to auto-detect.
    pub language: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Idle,
    Recording,
    Transcribing,
    Ready,
    Inserting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionRequest {
    pub id: u64,
    pub settings: Settings,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Decision<D> {
    Insert {
        id: u64,
        destination: D,
        text: String,
    },
    None,
}

struct Job<D> {
    id: u64,
    destination: D,
    settings: Settings,
    text: Option<String>,
    deadline: Instant,
}

pub struct Coordinator<D> {
    phase: Phase,
    job: Option<Job<D>>,
    next_id: u64,
}

impl<D> Default for Coordinator<D> {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            job: None,
            next_id: 0,
        }
    }
}

impl<D: Clone> Coordinator<D> {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn start(&mut self, settings: Settings, destination: D, now: Instant) -> Option<u64> {
        if self.phase != Phase::Idle {
            return None;
        }

        self.next_id = self.next_id.checked_add(1).expect("request ID exhausted");
        self.job = Some(Job {
            id: self.next_id,
            destination,
            settings,
            text: None,
            deadline: now + RECORDING_LIMIT,
        });
        self.phase = Phase::Recording;
        Some(self.next_id)
    }

    pub fn release(
        &mut self,
        id: u64,
        has_audio: bool,
        now: Instant,
    ) -> Option<TranscriptionRequest> {
        let job = self.job.as_mut()?;
        if self.phase != Phase::Recording || job.id != id || now >= job.deadline || !has_audio {
            if self.phase == Phase::Recording && job.id == id && (now >= job.deadline || !has_audio)
            {
                self.reset();
            }

            return None;
        }

        job.deadline = now + TRANSCRIPTION_LIMIT;
        self.phase = Phase::Transcribing;
        Some(TranscriptionRequest {
            id,
            settings: job.settings.clone(),
        })
    }

    pub fn transcript(&mut self, id: u64, text: String, now: Instant) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        if self.phase != Phase::Transcribing || job.id != id {
            return false;
        }

        if now >= job.deadline || text.trim().is_empty() {
            self.reset();
            return false;
        }

        job.text = Some(text);
        self.phase = Phase::Ready;
        true
    }

    pub fn begin_insert(&mut self, id: u64, destination_valid: bool) -> Decision<D> {
        if self.phase != Phase::Ready || self.job.as_ref().is_none_or(|job| job.id != id) {
            return Decision::None;
        }

        let mut job = self.job.take().unwrap();
        if !destination_valid {
            self.phase = Phase::Idle;
            return Decision::None;
        }

        let destination = job.destination.clone();
        let text = job.text.take().unwrap();
        self.job = Some(job);
        self.phase = Phase::Inserting;
        Decision::Insert {
            id,
            destination,
            text,
        }
    }

    pub fn insertion_result(&mut self, id: u64) {
        if self.phase == Phase::Inserting && self.job.as_ref().is_some_and(|job| job.id == id) {
            self.reset();
        }
    }

    pub fn cancel(&mut self, id: u64) -> bool {
        if self.phase == Phase::Inserting || self.job.as_ref().is_none_or(|job| job.id != id) {
            return false;
        }

        self.reset();
        true
    }

    fn reset(&mut self) {
        self.job = None;
        self.phase = Phase::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeInput {
        submissions: Vec<String>,
    }

    impl FakeInput {
        fn submit(&mut self, coordinator: &mut Coordinator<&str>, decision: Decision<&str>) {
            if let Decision::Insert { id, text, .. } = decision {
                self.submissions.push(text);
                coordinator.insertion_result(id);
            }
        }
    }

    fn settings() -> Settings {
        Settings {
            microphone_index: 2,
            speech: SpeechConfig::Local {
                adapter: "whisper.cpp".to_owned(),
                model_id: "base".to_owned(),
                artifact: "local-model.bin".into(),
            },
            language: None,
        }
    }

    #[test]
    fn one_job_snapshots_settings_and_rejects_overlap() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let mut configured = settings();
        let id = coordinator
            .start(configured.clone(), "textbox", now)
            .unwrap();
        assert!(coordinator.start(settings(), "other", now).is_none());
        configured.microphone_index = 9;
        let request = coordinator.release(id, true, now).unwrap();
        assert_eq!(request.settings.microphone_index, 2);
        assert_eq!(coordinator.phase(), Phase::Transcribing);
    }

    #[test]
    fn cancellation_drops_late_results() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        assert!(coordinator.cancel(id));
        assert!(coordinator.release(id, true, now).is_none());
        let next = coordinator.start(settings(), "new", now).unwrap();
        assert!(next > id);
        coordinator.release(next, true, now);
        assert!(coordinator.cancel(next));
        assert!(!coordinator.transcript(next, "late text".into(), now));
        assert_eq!(coordinator.begin_insert(next, true), Decision::None);
    }

    #[test]
    fn cancellation_after_transcription_prevents_submission() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, true, now);
        assert!(coordinator.transcript(id, "hello".into(), now));
        assert!(coordinator.cancel(id));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
        assert_eq!(coordinator.phase(), Phase::Idle);
    }

    #[test]
    fn unverified_destination_never_inserts() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, true, now);
        assert!(coordinator.transcript(id, "hello".into(), now));
        assert_eq!(coordinator.begin_insert(id, false), Decision::None);
        assert_eq!(coordinator.phase(), Phase::Idle);
    }

    #[test]
    fn insertion_is_submitted_at_most_once() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, true, now);
        assert!(coordinator.transcript(id, "hello".into(), now));
        let mut submissions = Vec::new();
        if let Decision::Insert { text, .. } = coordinator.begin_insert(id, true) {
            submissions.push(text);
        }
        assert!(!coordinator.cancel(id));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
        coordinator.insertion_result(id);
        coordinator.insertion_result(id);
        assert_eq!(submissions, vec!["hello"]);
    }

    #[test]
    fn stale_ids_and_deadlines_never_submit_to_a_new_job() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let mut input = FakeInput {
            submissions: Vec::new(),
        };
        let old = coordinator.start(settings(), "old", now).unwrap();
        assert!(
            coordinator
                .release(old, true, now + RECORDING_LIMIT)
                .is_none()
        );
        let current = coordinator.start(settings(), "current", now).unwrap();
        assert!(coordinator.release(old, true, now).is_none());
        assert!(!coordinator.cancel(old));
        assert!(coordinator.release(current, true, now).is_some());
        assert!(!coordinator.transcript(old, "stale".into(), now));
        assert!(coordinator.transcript(current, "current text".into(), now));
        let stale = coordinator.begin_insert(old, true);
        input.submit(&mut coordinator, stale);
        assert!(input.submissions.is_empty());
        let decision = coordinator.begin_insert(current, true);
        input.submit(&mut coordinator, decision);
        let duplicate = coordinator.begin_insert(current, true);
        input.submit(&mut coordinator, duplicate);
        assert_eq!(input.submissions, vec!["current text"]);
        assert_eq!(coordinator.phase(), Phase::Idle);
    }

    #[test]
    fn silence_empty_transcripts_and_timeouts_never_insert() {
        let now = Instant::now();
        let mut coordinator = Coordinator::<&str>::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        assert!(coordinator.release(id, false, now).is_none());
        assert_eq!(coordinator.phase(), Phase::Idle);
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, true, now);
        assert!(!coordinator.transcript(id, "  ".into(), now));
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, true, now);
        assert!(!coordinator.transcript(id, "late".into(), now + TRANSCRIPTION_LIMIT));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
    }
}
