//! Drives one run to completion: spawns the loop as a task, hands it a
//! [`Context`], and surfaces the transcript and the frame log back to the
//! caller.
//!
//! A run is an ordinary task on the caller's runtime. Whatever a tool call
//! or a granted function starts there — a background process's output, a
//! pooled connection — therefore outlives the run that started it.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::Instrument;

use crate::context::Context;
use crate::error::Error;
use crate::frame::{CoreEvent, FrameId};
use crate::mapping;
use crate::state::{Grant, HarnessState, Seed};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The loop ended on a failure it did not handle.
    #[error(transparent)]
    Loop(#[from] Error),
    /// The run was ended from outside ([`HarnessRun::terminate`]) before the
    /// loop finished.
    #[error("the run was terminated before it finished")]
    Terminated,
    #[error("the run panicked")]
    Panicked,
}

/// Everything a finished run leaves behind, beyond what already streamed.
pub struct RunOutcome {
    /// What happened, in order (`harness-events.md` §4).
    pub frames: Vec<CoreEvent>,
    /// The canonical lineage as this run left it — what seeds turn N+1
    /// (`history-abstract.md` H7). Equal to the run's own [`Seed`] when the
    /// loop never committed, since nothing moved the lineage.
    pub committed: Seed,
    /// The model frame whose commit produced [`Self::committed`], so a caller
    /// writing a spine can point it at the matching exchange. `None` when the
    /// loop never committed — there is then no position to record. A caller
    /// may also clear it to say the same thing about a run it does not want
    /// the lineage advanced from, which is what `crates/api` does with a run
    /// that ended in error.
    pub committed_frame: Option<FrameId>,
    /// What ended the run, if it did not finish. The frame log is recorded
    /// regardless and stays worth persisting — it is the only account of
    /// what reached the provider.
    pub error: Option<RunError>,
}

/// A running (or finished) run. `transcript` streams live; the frame log and
/// any run-level error are available together after [`HarnessRun::join`].
pub struct HarnessRun {
    pub transcript: UnboundedReceiver<serde_json::Value>,
    terminator: Terminator,
    task: tokio::task::JoinHandle<RunOutcome>,
}

/// A standalone kill switch for one run, handed out by
/// [`HarnessRun::terminator`]. Terminating a run that has already finished is
/// a no-op, so a stale one is harmless to hold.
#[derive(Clone)]
pub struct Terminator(Arc<Notify>);

impl Terminator {
    pub fn terminate(&self) {
        // `notify_one` keeps a permit when nothing is waiting yet, so a kill
        // that lands before the run's task first polls is not lost.
        self.0.notify_one();
    }
}

impl HarnessRun {
    /// Starts the conversational loop against `input`, under `grant`. `seed`
    /// is what a prior run committed — `Seed::default()` for a fresh
    /// conversation's first turn, since nothing in memory survives between
    /// runs.
    ///
    /// `input` is the turn's messages, plural. A turn is usually one message
    /// and often will be, but not always: a consumer that has something to
    /// tell the model alongside the user's own words — that an environment was
    /// added, say — has to put it *in* the conversation, and the alternative
    /// is editing the system prompt, which is prefix mutation and invalidates
    /// everything cached behind it.
    ///
    /// Must be called from within a tokio runtime.
    pub fn start(input: Vec<llm::Message>, grant: Grant, seed: Seed) -> HarnessRun {
        Self::start_with(crate::conversational::execute, input, grant, seed)
    }

    /// [`start`](Self::start), with `run` in place of the conversational
    /// loop. This is how the capabilities a loop is handed are exercised on
    /// their own.
    pub fn start_with<F, Fut>(
        run: F,
        input: Vec<llm::Message>,
        grant: Grant,
        seed: Seed,
    ) -> HarnessRun
    where
        F: FnOnce(Context, Vec<mapping::Message>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), Error>> + Send + 'static,
    {
        let (transcript_tx, transcript_rx) = tokio::sync::mpsc::unbounded_channel();
        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
        let kill = Arc::new(Notify::new());

        let state = Arc::new(Mutex::new(HarnessState::new(
            grant,
            seed,
            transcript_tx,
            frames_tx,
        )));
        let input: Vec<mapping::Message> = input.iter().map(mapping::Message::from).collect();

        let task = tokio::spawn({
            let kill = Arc::clone(&kill);
            async move {
                let context = Context::new(Arc::clone(&state));

                // Dropping the loop's future is what a kill does: every model
                // stream and tool call it was waiting on is dropped with it.
                // A panic is caught rather than left to the task, so a run
                // that died still hands back its frame log.
                let result = tokio::select! {
                    biased;
                    () = kill.notified() => Err(RunError::Terminated),
                    ended = AssertUnwindSafe(run(context, input)).catch_unwind() => match ended {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(RunError::Loop(error)),
                        Err(_) => Err(RunError::Panicked),
                    },
                };

                let mut frames = Vec::new();
                while let Ok(frame) = frames_rx.try_recv() {
                    frames.push(frame);
                }

                let harness = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                RunOutcome {
                    frames,
                    committed: Seed {
                        provider: harness.grant.client.provider().to_string(),
                        model: harness.grant.model.clone(),
                        messages: harness.lineage_iter().cloned().collect(),
                        options: harness.baseline.clone(),
                        context_tokens: harness.context_tokens,
                    },
                    committed_frame: harness.committed_frame.clone(),
                    error: result.err(),
                }
                // `state` drops here and takes the transcript's sender with
                // it, which is what ends the caller's `transcript.recv()`.
            }
            // The caller's span, so everything the run logs names its run.
            .instrument(tracing::Span::current())
        });

        HarnessRun {
            transcript: transcript_rx,
            terminator: Terminator(kill),
            task,
        }
    }

    /// Ends the run where it stands. Safe to call from anywhere, at any
    /// time — the backstop for a loop that does not stop when asked to
    /// ([`crate::interrupt`]). The run still hands back its frame log, with
    /// [`RunError::Terminated`] as its error.
    pub fn terminate(&self) {
        self.terminator.terminate();
    }

    /// The same kill, detached from the run handle so it can be held by
    /// something else while the run is being consumed.
    ///
    /// [`Self::terminate`] needs `&self`, and a caller that streams the
    /// transcript and then `join`s has given the handle away by the time it
    /// would want to use it. A watchdog waiting out an interrupt's grace
    /// period is exactly that caller.
    pub fn terminator(&self) -> Terminator {
        self.terminator.clone()
    }

    /// Waits for the run to finish and returns what it left behind.
    ///
    /// `Err` is reserved for a run that left nothing behind at all, which
    /// only a cancelled task does. A run whose *loop* failed returns `Ok`,
    /// carrying its frame log and lineage alongside the terminal error in
    /// [`RunOutcome::error`]: those frames are the record of what was
    /// actually sent to the provider, and the failing path is where that
    /// record is worth the most. Callers must check `error` — persisting the
    /// frames while refusing to advance lineage from a run that did not
    /// finish is the intended shape.
    pub async fn join(self) -> Result<RunOutcome, RunError> {
        self.task.await.map_err(|_| RunError::Panicked)
    }
}
