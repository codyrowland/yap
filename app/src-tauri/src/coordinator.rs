use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// What was dictated with what, for History.
#[derive(Clone, Debug, PartialEq)]
pub struct JobInfo {
    pub duration_seconds: f32,
    pub engine: String,
    pub model: String,
}

#[derive(Debug, PartialEq)]
pub enum Decision<D> {
    Insert {
        id: u64,
        destination: D,
        text: String,
        info: JobInfo,
    },
    None,
}

/// The last successful insertion, kept briefly so it can be undone.
#[derive(Clone, Debug, PartialEq)]
pub struct LastInsert<D> {
    pub id: u64,
    pub destination: D,
    pub at: Instant,
}

struct Job<D> {
    id: u64,
    destination: D,
    settings: Settings,
    text: Option<String>,
    deadline: Instant,
    audio_seconds: f32,
    cancelled: Arc<AtomicBool>,
}

pub struct Coordinator<D> {
    phase: Phase,
    job: Option<Job<D>>,
    next_id: u64,
    last_insert: Option<LastInsert<D>>,
    closed: bool,
}

impl<D> Default for Coordinator<D> {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            job: None,
            next_id: 0,
            last_insert: None,
            closed: false,
        }
    }
}

impl<D: Clone> Coordinator<D> {
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether a new job can start: nothing in progress and not shut down.
    pub fn idle(&self) -> bool {
        !self.closed && self.phase == Phase::Idle
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The job that is recording, transcribing or waiting to be inserted.
    pub fn active_id(&self) -> Option<u64> {
        match self.phase {
            Phase::Recording | Phase::Transcribing | Phase::Ready => {
                self.job.as_ref().map(|job| job.id)
            }
            Phase::Idle | Phase::Inserting => None,
        }
    }

    pub fn is_active(&self, id: u64) -> bool {
        self.active_id() == Some(id)
    }

    /// Where active job `id` will insert its text.
    pub fn destination(&self, id: u64) -> Option<&D> {
        if !self.is_active(id) {
            return None;
        }

        self.job.as_ref().map(|job| &job.destination)
    }

    /// Set when active job `id` is cancelled, so its work can stop early.
    pub fn cancel_flag(&self, id: u64) -> Option<Arc<AtomicBool>> {
        if !self.is_active(id) {
            return None;
        }

        self.job.as_ref().map(|job| Arc::clone(&job.cancelled))
    }

    pub fn start(&mut self, settings: Settings, destination: D, now: Instant) -> Option<u64> {
        if !self.idle() {
            return None;
        }

        self.next_id = self.next_id.checked_add(1).expect("request ID exhausted");
        self.job = Some(Job {
            id: self.next_id,
            destination,
            settings,
            text: None,
            deadline: now + RECORDING_LIMIT,
            audio_seconds: 0.0,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        self.last_insert = None;
        self.phase = Phase::Recording;
        Some(self.next_id)
    }

    /// Ends recording `id`. `audio_seconds` is `None` when nothing worth
    /// transcribing was recorded.
    pub fn release(
        &mut self,
        id: u64,
        audio_seconds: Option<f32>,
        now: Instant,
    ) -> Option<TranscriptionRequest> {
        let job = self.job.as_mut()?;
        let has_audio = audio_seconds.is_some();
        if self.phase != Phase::Recording || job.id != id || now >= job.deadline || !has_audio {
            if self.phase == Phase::Recording && job.id == id && (now >= job.deadline || !has_audio)
            {
                self.reset();
            }

            return None;
        }

        job.audio_seconds = audio_seconds.unwrap_or_default();
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
        let (engine, model) = job.settings.speech.describe();
        let info = JobInfo {
            duration_seconds: job.audio_seconds,
            engine,
            model,
        };
        self.job = Some(job);
        self.phase = Phase::Inserting;
        Decision::Insert {
            id,
            destination,
            text,
            info,
        }
    }

    pub fn insertion_result(&mut self, id: u64, inserted: bool, now: Instant) {
        if self.phase != Phase::Inserting || self.job.as_ref().is_none_or(|job| job.id != id) {
            return;
        }

        if inserted && let Some(job) = self.job.as_ref() {
            self.last_insert = Some(LastInsert {
                id,
                destination: job.destination.clone(),
                at: now,
            });
        }
        self.reset();
    }

    pub fn take_last_insert(&mut self) -> Option<LastInsert<D>> {
        self.last_insert.take()
    }

    /// Cancels job `id` and signals its work to stop. An insertion already
    /// under way can't be cancelled.
    pub fn cancel(&mut self, id: u64) -> bool {
        if !self.is_active(id) {
            return false;
        }

        self.signal_cancel();
        self.reset();
        true
    }

    /// Cancels whatever job is active.
    pub fn cancel_current(&mut self) -> bool {
        match self.active_id() {
            Some(id) => self.cancel(id),
            None => false,
        }
    }

    /// Drops job `id` after an error. Unlike `cancel`, the failure is still
    /// reported.
    pub fn fail(&mut self, id: u64) {
        if self.is_active(id) {
            self.reset();
        }
    }

    /// Cancels everything and refuses new jobs; for when Yap exits.
    pub fn shutdown(&mut self) {
        self.closed = true;
        self.signal_cancel();
        self.last_insert = None;
        self.reset();
    }

    fn signal_cancel(&self) {
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Relaxed);
        }
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
                coordinator.insertion_result(id, true, Instant::now());
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
        let request = coordinator.release(id, Some(1.0), now).unwrap();
        assert_eq!(request.settings.microphone_index, 2);
        assert_eq!(coordinator.phase(), Phase::Transcribing);
    }

    #[test]
    fn cancellation_drops_late_results() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        assert!(coordinator.cancel(id));
        assert!(coordinator.release(id, Some(1.0), now).is_none());
        let next = coordinator.start(settings(), "new", now).unwrap();
        assert!(next > id);
        coordinator.release(next, Some(1.0), now);
        assert!(coordinator.cancel(next));
        assert!(!coordinator.transcript(next, "late text".into(), now));
        assert_eq!(coordinator.begin_insert(next, true), Decision::None);
    }

    #[test]
    fn cancellation_after_transcription_prevents_submission() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
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
        coordinator.release(id, Some(1.0), now);
        assert!(coordinator.transcript(id, "hello".into(), now));
        assert_eq!(coordinator.begin_insert(id, false), Decision::None);
        assert_eq!(coordinator.phase(), Phase::Idle);
    }

    #[test]
    fn insertion_is_submitted_at_most_once() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
        assert!(coordinator.transcript(id, "hello".into(), now));
        let mut submissions = Vec::new();
        if let Decision::Insert { text, .. } = coordinator.begin_insert(id, true) {
            submissions.push(text);
        }
        assert!(!coordinator.cancel(id));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
        coordinator.insertion_result(id, true, now);
        coordinator.insertion_result(id, true, now);
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
                .release(old, Some(1.0), now + RECORDING_LIMIT)
                .is_none()
        );
        let current = coordinator.start(settings(), "current", now).unwrap();
        assert!(coordinator.release(old, Some(1.0), now).is_none());
        assert!(!coordinator.cancel(old));
        assert!(coordinator.release(current, Some(1.0), now).is_some());
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
        assert!(coordinator.release(id, None, now).is_none());
        assert_eq!(coordinator.phase(), Phase::Idle);
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
        assert!(!coordinator.transcript(id, "  ".into(), now));
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
        assert!(!coordinator.transcript(id, "late".into(), now + TRANSCRIPTION_LIMIT));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
    }

    #[test]
    fn job_is_active_until_insertion_begins() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        assert_eq!(coordinator.active_id(), None);
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        assert!(coordinator.is_active(id));
        assert_eq!(coordinator.destination(id), Some(&"textbox"));
        coordinator.release(id, Some(1.0), now);
        assert!(coordinator.is_active(id));
        coordinator.transcript(id, "hello".into(), now);
        assert!(coordinator.is_active(id));
        coordinator.begin_insert(id, true);
        assert_eq!(coordinator.phase(), Phase::Inserting);
        assert!(!coordinator.is_active(id));
        assert_eq!(coordinator.destination(id), None);
        assert!(!coordinator.cancel_current());
        coordinator.insertion_result(id, true, now);
        assert!(coordinator.idle());
    }

    #[test]
    fn insertion_carries_job_info_and_remembers_the_last_insert() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(2.5), now);
        coordinator.transcript(id, "hello".into(), now);
        let Decision::Insert { info, .. } = coordinator.begin_insert(id, true) else {
            panic!("expected an insertion");
        };
        assert_eq!(info.duration_seconds, 2.5);
        assert_eq!(info.engine, "On-device");
        assert_eq!(info.model, "base");
        coordinator.insertion_result(id, true, now);
        assert_eq!(
            coordinator.take_last_insert(),
            Some(LastInsert {
                id,
                destination: "textbox",
                at: now,
            })
        );
        assert_eq!(coordinator.take_last_insert(), None);

        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
        coordinator.transcript(id, "hello".into(), now);
        coordinator.begin_insert(id, true);
        coordinator.insertion_result(id, false, now);
        assert_eq!(coordinator.take_last_insert(), None);
    }

    #[test]
    fn starting_a_job_forgets_the_last_insert() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        coordinator.release(id, Some(1.0), now);
        coordinator.transcript(id, "hello".into(), now);
        coordinator.begin_insert(id, true);
        coordinator.insertion_result(id, true, now);
        coordinator.start(settings(), "textbox", now).unwrap();
        assert_eq!(coordinator.take_last_insert(), None);
    }

    #[test]
    fn cancel_signals_the_job_but_failure_does_not() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        let cancelled = coordinator.cancel_flag(id).unwrap();
        assert!(coordinator.cancel_current());
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(coordinator.cancel_flag(id).is_none());

        let id = coordinator.start(settings(), "textbox", now).unwrap();
        let cancelled = coordinator.cancel_flag(id).unwrap();
        coordinator.release(id, Some(1.0), now);
        coordinator.fail(id);
        assert!(!cancelled.load(Ordering::Relaxed));
        assert!(coordinator.idle());
    }

    #[test]
    fn shutdown_cancels_the_job_and_refuses_new_ones() {
        let now = Instant::now();
        let mut coordinator = Coordinator::default();
        let id = coordinator.start(settings(), "textbox", now).unwrap();
        let cancelled = coordinator.cancel_flag(id).unwrap();
        coordinator.release(id, Some(1.0), now);
        coordinator.shutdown();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(coordinator.closed());
        assert!(!coordinator.idle());
        assert!(!coordinator.transcript(id, "late".into(), now));
        assert_eq!(coordinator.begin_insert(id, true), Decision::None);
        assert!(coordinator.start(settings(), "textbox", now).is_none());
    }
}
