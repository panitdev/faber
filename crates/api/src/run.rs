//! Driving a harness run from the API, and getting what it produced onto disk
//! and out to a subscriber.
//!
//! Three things happen here that the rest of `crates/api` does not do:
//!
//! - A [`llm::ModelClient`] is constructed from a user's model row and their
//!   decrypted key ([`build_client`]).
//! - A harness executes in-process ([`spawn_run`]), streaming what it yields
//!   into both the `transcript` table and a `broadcast` channel.
//! - The two logs `history-abstract.md` H2 keeps separate are written from
//!   their two separate sources: the transcript from what the harness yielded,
//!   the exchange from the frame log Core recorded at the capability boundary.
//!   Neither derives the other, and the harness cannot forge the second.
//!
//! Cross-turn history (H7) closes here too: a run's committed lineage is
//! serialized into a `blob`, named by the `exchange` its commit came from, and
//! reached again on the next turn through the thread's `spine`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use diesel::{ExpressionMethods, JoinOnDsl, QueryDsl, SelectableHelper};
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use harness::frame::{CoreEvent, FrameDetail, FrameId, Outcome};
use harness::{Grant, HarnessRun, Interrupt, Interrupter, RunOutcome, Seed};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    compact::{Compactor, KIND_MESSAGE},
    db::DbPool,
    error::{ApiResult, AppError},
    models::{
        blob::NewBlob,
        exchange::NewExchange,
        model_config::{ModelConfig, Wire},
        now_epoch,
        session::UpdateSession,
        spine::NewSpine,
        transcript::NewTranscript,
    },
    schema::{blob, exchange, run, spine, thread, transcript},
    state::AppState,
};

/// How many events a subscriber may fall behind before it is told to re-sync.
///
/// A slow client must never stall the run that is feeding it, so the channel
/// drops rather than blocks; the subscriber sees `Lagged` and refetches
/// through `GET /api/runs/{id}/transcript`, which is durable and complete.
const BROADCAST_CAPACITY: usize = 1024;

/// Live subscribers, keyed by session.
///
/// In-process only: a subscriber reaches a run just when it connected to the
/// instance that owns it. With one instance that is total; with more it is
/// not, and the fix is a shared bus rather than a bigger map.
pub type RunRegistry = Arc<RwLock<HashMap<Uuid, SessionChannel>>>;

/// One session's channel, and how many runs are still publishing to it.
///
/// The count is what makes an entry safe to drop. Removing a channel a run
/// still holds does not stop that run — it keeps publishing into its own
/// clone — but it does route the *next* subscriber to a different channel,
/// which would show an empty stream while a run was plainly in progress.
pub struct SessionChannel {
    sender: broadcast::Sender<StreamEvent>,
    active_runs: usize,
}

impl SessionChannel {
    fn new() -> Self {
        Self {
            sender: broadcast::channel(BROADCAST_CAPACITY).0,
            active_runs: 0,
        }
    }

    /// Nothing is publishing and nobody is listening.
    fn is_inert(&self) -> bool {
        self.active_runs == 0 && self.sender.receiver_count() == 0
    }
}

/// One event on the wire to a subscriber.
///
/// `(run_id, seq)` together are the cursor, because `transcript.seq` is unique
/// per *run* (`UNIQUE (run_id, seq)`), not per session — a session-scoped
/// stream carries several runs' worth of independently-numbered events.
#[derive(Clone, Debug, Serialize)]
pub struct StreamEvent {
    pub run_id: Uuid,
    pub seq: i64,
    /// Free-form, deliberately not an enum on either side
    /// (`history-abstract.md` H8.7). Harness events carry their own `type`;
    /// this layer adds `input` and the two terminal markers.
    pub kind: String,
    pub payload: Value,
}

/// Terminal marker: the run finished and nothing more will arrive for it.
pub const KIND_RUN_END: &str = "run_end";
/// Terminal marker: the run ended in a failure the harness did not handle.
pub const KIND_RUN_ERROR: &str = "run_error";
/// Terminal marker: the run stopped because someone asked it to.
///
/// Its own marker rather than a `run_error` with a special message: a user who
/// pressed stop got what they asked for, and a client that renders that as a
/// failure is telling them something went wrong when nothing did.
pub const KIND_RUN_INTERRUPTED: &str = "run_interrupted";
/// The user's own turn. Not harness-yielded, but H2 makes the transcript the
/// user-facing conversation, and a conversation missing one side of itself is
/// not one.
pub const KIND_INPUT: &str = "input";

/// The session said something, rather than the user or the model: an
/// environment was added, or one it has could not be reached this run. Its own
/// kind so a client renders it as what it is — neither side's words.
pub const KIND_ENVIRONMENTS: &str = "environments";

/// One of a turn's input messages, and what the transcript calls it.
///
/// The kind is carried rather than derived from the role. It used to be read
/// off `Role::System`, which stopped distinguishing anything the moment the
/// environment announcement had to become a user turn to survive the Anthropic
/// wire — and a note that silently rendered as a second user bubble is worse
/// than no note.
pub struct TurnMessage {
    pub kind: &'static str,
    pub message: llm::Message,
}

/// The `transcript.seq` the turn's first input message occupies. Harness
/// output starts after the last of them.
pub const INPUT_SEQ: i64 = 0;

// ---------------------------------------------------------------------------
// Model clients
// ---------------------------------------------------------------------------

/// Builds a client for a user's model row.
///
/// `http: None` is deliberate. `AppState::http` carries a 10-second total
/// request timeout, and both provider modules warn that a total timeout cuts a
/// long generation off mid-answer; `crates/llm` builds its own with a connect
/// and a read timeout instead, which bounds a stalled connection without
/// bounding the generation.
pub fn build_client(config: &ModelConfig, api_key: String) -> ApiResult<Arc<dyn llm::ModelClient>> {
    let base_url = url::Url::parse(&config.base_url).map_err(|_| {
        AppError::BadRequest(format!("model {} has an invalid base_url", config.alias))
    })?;
    let key = secrecy::SecretString::from(api_key);

    let wire = Wire::from_db(&config.wire).ok_or_else(|| {
        AppError::BadRequest(format!("model {} has an unknown wire", config.alias))
    })?;

    let client: Arc<dyn llm::ModelClient> = match wire {
        Wire::Anthropic => Arc::new(
            llm::anthropic::Anthropic::new(llm::anthropic::Config {
                api_key: key,
                base_url: Some(base_url),
                betas: Vec::new(),
                http: None,
            })
            .map_err(|error| {
                tracing::error!(error = %error, "failed to build anthropic client");
                AppError::Internal
            })?,
        ),
        Wire::Openai => Arc::new(
            llm::openai::OpenAI::new(llm::openai::Config {
                api_key: key,
                base_url: Some(base_url),
                http: None,
            })
            .map_err(|error| {
                tracing::error!(error = %error, "failed to build openai client");
                AppError::Internal
            })?,
        ),
    };

    Ok(client)
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// The thread's canonical lineage, for the next run to start from.
///
/// Walks the last `spine` position to its exchange and reads the lineage blob
/// that exchange's commit produced. A thread with no spine, or whose newest
/// position predates this mechanism, yields `Seed::default()` — turn one,
/// which is also the correct answer for a lineage that cannot be recovered:
/// starting fresh is a visible loss of context, whereas a half-reconstructed
/// lineage is a silent one.
pub async fn load_seed(conn: &mut AsyncPgConnection, thread_id: Uuid) -> ApiResult<Seed> {
    let digest: Option<Vec<u8>> = spine::table
        .inner_join(exchange::table.on(exchange::id.eq(spine::exchange_id)))
        .filter(spine::thread_id.eq(thread_id))
        .order(spine::seq.desc())
        .select(exchange::canonical_blob_digest)
        .first::<Option<Vec<u8>>>(conn)
        .await
        .optional_row()
        .map_err(|err| AppError::db(err, "run.load_seed.spine_tail"))?
        .flatten();

    let Some(digest) = digest else {
        return Ok(Seed::default());
    };

    let data: Option<Vec<u8>> = blob::table
        .filter(blob::digest.eq(&digest))
        .select(blob::data)
        .first::<Option<Vec<u8>>>(conn)
        .await
        .optional_row()
        .map_err(|err| AppError::db(err, "run.load_seed.blob"))?
        .flatten();

    let Some(data) = data else {
        tracing::warn!(%thread_id, "canonical lineage blob is missing its data; seeding empty");
        return Ok(Seed::default());
    };

    match serde_json::from_slice::<Seed>(&data) {
        Ok(seed) => Ok(seed),
        Err(error) => {
            tracing::error!(%thread_id, error = %error, "canonical lineage failed to decode; seeding empty");
            Ok(Seed::default())
        }
    }
}

/// `diesel::result::Error::NotFound` means "no row", which is not a failure
/// for any of the tail lookups above.
trait OptionalRow<T> {
    fn optional_row(self) -> Result<Option<T>, diesel::result::Error>;
}

impl<T> OptionalRow<T> for Result<T, diesel::result::Error> {
    fn optional_row(self) -> Result<Option<T>, diesel::result::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(diesel::result::Error::NotFound) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

/// Everything one run needs, assembled by the route before it detaches.
pub struct RunRequest {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub thread_id: Uuid,
    /// Whose environments this run may reach. Bindings are per user, and a
    /// session's are resolved against them at run time rather than trusted
    /// from the row.
    pub user_id: Uuid,
    pub config: ModelConfig,
    pub api_key: String,
    /// The turn's messages, in order. Usually just the user's; more when the
    /// session had something to say alongside it, such as an environment
    /// having been added.
    pub input: Vec<TurnMessage>,
    /// The run's half of the stop signal, from [`open_interrupt`].
    pub interrupt: Interrupt,
}

/// Claims the session's channel for a run that is about to start.
///
/// Called before the POST returns, so a subscriber arriving in the window
/// between the response and the harness's first event still finds the channel
/// the run will publish to. Paired with [`close_run`].
pub fn open_run(registry: &RunRegistry, session_id: Uuid) -> broadcast::Sender<StreamEvent> {
    let mut guard = registry.write().expect("run registry lock poisoned");
    guard.retain(|id, channel| *id == session_id || !channel.is_inert());
    let channel = guard.entry(session_id).or_insert_with(SessionChannel::new);
    channel.active_runs += 1;
    channel.sender.clone()
}

/// Releases a run's claim, dropping the channel once nothing needs it.
pub fn close_run(registry: &RunRegistry, session_id: Uuid) {
    let mut guard = registry.write().expect("run registry lock poisoned");
    if let Some(channel) = guard.get_mut(&session_id) {
        channel.active_runs = channel.active_runs.saturating_sub(1);
        if channel.is_inert() {
            guard.remove(&session_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Interruption
// ---------------------------------------------------------------------------

/// Every run this instance can still stop, keyed by run — not by session, the
/// way [`RunRegistry`] is. A stop names one run: a session may have several in
/// flight, and stopping the wrong one is worse than stopping none.
///
/// In-process, and for the same reason and with the same consequence as
/// [`RunRegistry`]: an interrupt that lands on an instance which does not own
/// the run cannot reach it, and says so rather than pretending. The fix is the
/// same shared bus, not a bigger map.
pub type InterruptRegistry = Arc<RwLock<HashMap<Uuid, Interrupter>>>;

/// How long a stopped run is left to wind itself up before the isolate is
/// killed.
///
/// The signal is the mechanism and the kill is the backstop
/// (`history-abstract.md` H9.3), so this window is what separates them. It has
/// to be long enough for a harness to notice a rejected call, commit what it
/// has, and yield a closing event — and short enough that a harness which
/// cannot notice, because it is busy in its own JavaScript, still stops while
/// the user is watching.
const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Registers a run as interruptible and hands back the run's half of the
/// signal.
///
/// Called before the POST returns, like [`open_run`] and for the same reason:
/// the response carries the `run_id` a client interrupts by, so the moment it
/// has one, a stop for it must land somewhere. Paired with
/// [`close_interrupt`].
pub fn open_interrupt(registry: &InterruptRegistry, run_id: Uuid) -> Interrupt {
    let (interrupter, interrupt) = harness::interrupt();
    registry
        .write()
        .expect("interrupt registry lock poisoned")
        .insert(run_id, interrupter);
    interrupt
}

/// Forgets a finished run. After this a stop for it is answered from
/// `run.completed_at` instead — the run is over, and the durable record is
/// what says so.
pub fn close_interrupt(registry: &InterruptRegistry, run_id: Uuid) {
    registry
        .write()
        .expect("interrupt registry lock poisoned")
        .remove(&run_id);
}

/// Raises the stop for a run this instance owns. `false` when it owns no such
/// run — either it finished, or it belongs to another instance, which the
/// caller distinguishes because only it knows what the database says.
pub fn raise_interrupt(registry: &InterruptRegistry, run_id: Uuid) -> bool {
    let guard = registry.read().expect("interrupt registry lock poisoned");
    match guard.get(&run_id) {
        Some(interrupter) => {
            interrupter.raise();
            true
        }
        None => false,
    }
}

/// Subscribes to a session's live events.
///
/// Creates the channel if there isn't one, rather than handing back an empty
/// stream: opening the stream *before* sending the first message is the normal
/// order for a UI, and a subscriber that connected first must not be the one
/// that misses the run.
pub fn subscribe(registry: &RunRegistry, session_id: Uuid) -> broadcast::Receiver<StreamEvent> {
    let mut guard = registry.write().expect("run registry lock poisoned");
    guard.retain(|id, channel| *id == session_id || !channel.is_inert());
    guard
        .entry(session_id)
        .or_insert_with(SessionChannel::new)
        .sender
        .subscribe()
}

/// Runs a harness to completion in the background.
///
/// Detached on purpose: the POST that started it has already returned, and the
/// run must survive both that response and any subscriber disconnecting. The
/// only way to observe it afterwards is the transcript — live or persisted.
/// `sender` is the one [`open_run`] already handed the caller, so the claim is
/// taken exactly once and before this task is scheduled.
pub fn spawn_run(state: AppState, sender: broadcast::Sender<StreamEvent>, request: RunRequest) {
    tokio::spawn(async move {
        let session_id = request.session_id;
        let run_id = request.run_id;
        let thread_id = request.thread_id;
        let interrupt = request.interrupt.clone();

        let result = execute(&state, &sender, request).await;

        // A stopped run almost always *also* ends in error — the rejected
        // call propagates out of the harness, or the isolate was killed
        // outright — so the stop is read first. It is the cause, and the error
        // is its shadow; reporting the shadow would tell the user their run
        // broke when they are the one who stopped it.
        let terminal = match (&result, interrupt.raised()) {
            (_, true) => {
                if let Err(error) = &result {
                    tracing::info!(%run_id, error = %error, "interrupted run ended in error");
                }
                (KIND_RUN_INTERRUPTED.to_owned(), Value::Null)
            }
            (Ok(_), false) => (KIND_RUN_END.to_owned(), Value::Null),
            (Err(error), false) => {
                tracing::error!(%run_id, error = %error, "harness run failed");
                (
                    KIND_RUN_ERROR.to_owned(),
                    json!({ "message": error.to_string() }),
                )
            }
        };

        // The terminal's `seq` is the run's final live seq: it must land after
        // every event a live client already applied, which is the only value
        // the DB alone cannot reconstruct — folded events consume seqs without
        // persisting them. `execute` returns the exact counter; the error path
        // falls back to the last persisted row, which is the best available
        // approximation when the harness never finished streaming.
        let final_seq = match &result {
            Ok((_, seq)) => *seq,
            Err(_) => last_transcript_seq(&state, run_id).await,
        };

        let needs_recovery = match &result {
            Ok((committed, _)) => !committed,
            Err(_) => true,
        };
        if needs_recovery {
            if let Err(recovery_error) = record_recovery_history(&state.db, run_id, thread_id).await
            {
                tracing::error!(
                    %run_id,
                    error = %recovery_error,
                    "failed to preserve incomplete assistant text"
                );
            }
        }

        if let Err(error) =
            publish_terminal(&state, &sender, run_id, final_seq, terminal.0, terminal.1).await
        {
            tracing::error!(%run_id, error = %error, "failed to persist terminal run event");
        }

        // Stamped whether the run succeeded or failed — `completed_at` records
        // that the run is over, not that it went well, and it is the durable
        // terminator a client that reconnects after the fact reads.
        if let Err(error) = mark_run_complete(&state.db, run_id).await {
            tracing::error!(%run_id, error = %error, "failed to stamp run completion");
        }

        // Released here rather than anywhere earlier: right up until the row
        // is stamped, a stop is still something a user could reasonably ask
        // for, and after it there is nothing left to stop.
        close_interrupt(&state.interrupts, run_id);

        // Dropped before the claim is released, so the count is the only thing
        // keeping the entry alive by the time `close_run` inspects it.
        drop(sender);
        close_run(&state.runs, session_id);
    });
}

/// Runs a harness turn to completion, returning whether the exchange committed
/// and the run's final live `seq`. The `seq` is the one the last streamed event
/// occupied; the terminal (see [`publish_terminal`]) is published strictly after
/// it, so the persist-to-DB fallback never risks colliding with a seq the live
/// client has already applied.
async fn execute(
    state: &AppState,
    sender: &broadcast::Sender<StreamEvent>,
    request: RunRequest,
) -> ApiResult<(bool, i64)> {
    let RunRequest {
        run_id,
        session_id,
        thread_id,
        user_id,
        config,
        api_key,
        input,
        interrupt,
    } = request;

    // A stop can arrive before any of this: the POST returned the `run_id`
    // and detached, so the window between that and the isolate booting is a
    // real one a user can hit. Nothing has been sent to a provider yet, which
    // makes this the cheapest place the answer is ever available. The input
    // seqs through `INPUT_SEQ + input.len() - 1` were already persisted, so
    // the terminal lands right after them.
    if interrupt.raised() {
        return Ok((false, INPUT_SEQ + input.len() as i64));
    }

    let client = build_client(&config, api_key)?;

    let seed = {
        let mut conn = state.db.get().await?;
        load_seed(&mut conn, thread_id).await?
    };

    // Probed here rather than when the environment was tagged: a probe is a
    // network round trip, the POST that tags one has to stay fast, and a
    // machine that answered when it was tagged may not answer now.
    //
    // The blob store is this run's own. Captured output is a span the tool
    // surface redeems while rendering, and nothing outside the run reads one
    // yet — the day the exchange carries spans too, this becomes the store
    // behind `blob`, and nothing above it changes.
    let blobs: Arc<dyn environment::Blobs> = Arc::new(environment::MemoryBlobs::new());
    let bound =
        crate::environments::bind_session(&state, user_id, session_id, Arc::clone(&blobs)).await?;

    // Said out loud rather than swallowed, and to the user rather than only to
    // the log. A target the session has and this run could not reach is
    // missing from the registry, so a call against it answers "not bound" —
    // which is true of the run and misleading about the session. The user is
    // the one who can tell those apart and the only one who can fix it.
    for (label, reason) in &bound.unreachable {
        tracing::warn!(%run_id, %label, %reason, "an environment could not be bound for this run");
    }

    let surface = Arc::new(harness::Surface::new(Arc::new(bound.registry), blobs));

    // Two projections, one grant. The environment surface is always here; the
    // web surface only when this service was given an engine, because an
    // ungranted tool is absent rather than present and refusing. Unlike the
    // bindings, that cannot change mid-run, so the prefix stays constant for
    // the whole of it either way.
    let mut toolbox = harness::Toolbox::new();
    toolbox.add(
        harness::Surface::definitions(),
        Arc::clone(&surface).invoker(),
    );
    // Presentation policy and persistence are API-owned. The harness merely
    // carries this projection alongside its other generic tool surfaces.
    let present = Arc::new(crate::presentation::tool::PresentTool::new(
        state.clone(),
        user_id,
        session_id,
    ));
    toolbox.add(
        crate::presentation::tool::PresentTool::definitions(),
        present.invoker(),
    );
    if let Some(engine) = &state.search {
        let web = Arc::new(harness::Web::new(Arc::clone(engine)));
        toolbox.add(harness::Web::definitions(), web.invoker());
    }

    let mut functions = harness::FunctionRegistry::new();
    let db = state.db.clone();
    functions.register("session.get_title", {
        let db = db.clone();
        move |_input| {
            let db = db.clone();
            Box::pin(async move {
                let mut conn = db.get().await.map_err(|error| error.to_string())?;
                let title = crate::schema::session::table
                    .filter(crate::schema::session::id.eq(session_id))
                    .select(crate::schema::session::title)
                    .first::<Option<String>>(&mut conn)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(title.map_or(serde_json::Value::Null, serde_json::Value::String))
            })
        }
    });
    functions.register("session.set_title", move |input| {
        let db = db.clone();
        Box::pin(async move {
            let title = input
                .as_str()
                .ok_or_else(|| "session.set_title expects a string".to_owned())?
                .trim()
                .to_owned();
            if title.is_empty() {
                return Err("title cannot be empty".into());
            }
            if title.chars().count() > 200 {
                return Err("title must be 200 characters or fewer".into());
            }

            let mut conn = db.get().await.map_err(|error| error.to_string())?;
            diesel::update(
                crate::schema::session::table.filter(crate::schema::session::id.eq(session_id)),
            )
            .set(UpdateSession {
                title: Some(Some(title.as_str())),
                closed_at: None,
            })
            .execute(&mut conn)
            .await
            .map_err(|error| error.to_string())?;
            Ok(serde_json::Value::Null)
        })
    });

    let grant = Grant {
        client,
        model: config.wire_id.clone(),
        // A property of the endpoint behind this row, not of the wire: some
        // models reject a thinking turn replayed without its signature, others
        // reject the reasoning outright. Unset leaves the wire's own default.
        reasoning_history: config.reasoning_history(),
        advanced_options: config.advanced_options(),
        // Granted, not implemented: the surface is the standard projection of
        // the environment contract, and a harness gets a working environment
        // by being handed it. An ungranted tool is simply absent from `ctx` —
        // control by subtraction — so a session with no environments still
        // runs, with a loop that has fewer moves.
        tools: toolbox.definitions(),
        tool_invoker: Some(toolbox.invoker()),
        commit_granted: true,
        functions,
        // Granted like everything else here: a run that was handed no
        // interrupt is one nobody can stop, and the harness sees a stop only
        // as a call of its own failing.
        interrupt: Some(interrupt.clone()),
    };

    let mut first_harness_seq = INPUT_SEQ + input.len() as i64;
    for (label, reason) in &bound.unreachable {
        publish(
            state,
            sender,
            run_id,
            &mut first_harness_seq,
            KIND_ENVIRONMENTS.to_owned(),
            notice(&format!(
                "'{label}' could not be reached for this run: {reason}"
            )),
            true,
        )
        .await?;
    }

    let started_at = now_epoch();
    let mut run = HarnessRun::start(
        harness::harness_for(config.family.as_deref()),
        input.into_iter().map(|turn| turn.message).collect(),
        grant,
        seed,
    );

    // The backstop, armed for the whole run. A harness that never touches an
    // op after the stop — one looping in its own JavaScript, or one catching
    // the failure and refusing to end — is unreachable by the signal, and
    // `history-abstract.md` H9.3 is explicit that killing the isolate is what
    // covers that case. Aborted below once the run is over, so a run that
    // stopped itself in time is never killed after the fact.
    let watchdog = tokio::spawn({
        let terminator = run.terminator();
        let interrupt = interrupt.clone();
        async move {
            interrupt.raised_at().await;
            tokio::time::sleep(INTERRUPT_GRACE).await;
            tracing::warn!(%run_id, "harness did not stop within the grace period; killing the isolate");
            terminator.terminate();
        }
    });

    // Harness output starts after this turn's own input messages.
    //
    // Every event is published live and given a `seq`, so the SSE resume
    // cursor stays gap-free; only what [`Compactor`] calls durable is
    // written. The persisted rows therefore have holes in `seq`, which the
    // `seq > after_seq` reads and the client's dedupe-by-seq both already
    // tolerate.
    let mut seq = first_harness_seq;
    let mut compactor = Compactor::new();
    while let Some(event) = run.transcript.recv().await {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();

        let folded = compactor.push(&kind, &event);

        if let Some(message) = folded.flushed {
            publish(
                state,
                sender,
                run_id,
                &mut seq,
                KIND_MESSAGE.to_owned(),
                message,
                true,
            )
            .await?;
        }
        publish(
            state,
            sender,
            run_id,
            &mut seq,
            kind.clone(),
            event.clone(),
            folded.persist_raw,
        )
        .await?;
        if let Some(message) = folded.completed {
            publish(
                state,
                sender,
                run_id,
                &mut seq,
                KIND_MESSAGE.to_owned(),
                message,
                true,
            )
            .await?;
        }
    }

    // Before the join, not after: the text a harness streamed is what the user
    // saw, and it belongs in the transcript at the position it arrived —
    // whether or not the run goes on to end in error.
    if let Some(message) = compactor.finish() {
        publish(
            state,
            sender,
            run_id,
            &mut seq,
            KIND_MESSAGE.to_owned(),
            message,
            true,
        )
        .await?;
    }

    // `join` blocks on an OS thread handle, which must not happen on the async
    // runtime's worker.
    let joined = tokio::task::spawn_blocking(move || run.join()).await;

    // Disarmed before either error is propagated: the run is over either way,
    // and a watchdog outliving it would sit holding a handle to a dead isolate
    // for as long as the grace period.
    watchdog.abort();

    let mut outcome = joined
        .map_err(|error| {
            tracing::error!(%run_id, error = %error, "harness join task panicked");
            AppError::Internal
        })?
        .map_err(|error| AppError::Harness(error.to_string()))?;

    let error = outcome.error.as_ref().map(ToString::to_string);

    if let Some(error) = &error {
        if interrupt.raised() {
            tracing::info!(%run_id, error = %error, "harness run ended at a stop");
        } else {
            tracing::error!(%run_id, error = %error, "harness run ended in error");
        }

        // The frame log is kept for diagnosis — a failed harness still recorded
        // the exact provider exchange for every call it opened, and that is
        // precisely the path where those bytes are worth having. The *lineage*
        // is not advanced with it: a run that ends in error hands the thread's
        // position to `record_recovery_history`, which is the sole writer for
        // that path. Dropping the frame here is what keeps the two from each
        // claiming a `seq` for one run.
        outcome.committed_frame = None;
    }

    let committed = record_history(&state.db, run_id, thread_id, started_at, outcome)
        .await
        .inspect_err(|history_error| {
            // Logged against the harness error rather than replacing it: the
            // run's own cause is the more useful of the two, and it is about to
            // be swallowed by this `?`.
            if let Some(error) = &error {
                tracing::error!(
                    %run_id,
                    error = %error,
                    history_error = %history_error,
                    "the frame log of a failed run could not be persisted"
                );
            }
        })?;

    match error {
        Some(error) => Err(AppError::Harness(error)),
        None => Ok((committed, seq)),
    }
}

/// A transcript-only message from the session, in the shape a client already
/// reads. Never sent to the model: it describes this run's reach, and the
/// model's own answer to that question is the call it makes.
fn notice(text: &str) -> Value {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
    })
}

/// Publishes one event at the next `seq`, writing it first when it is durable.
///
/// Persist-then-publish is what keeps a subscriber from seeing an event a
/// reconnect would fail to replay. For a *folded* event that guarantee is
/// weaker by construction: a delta is published and never written, and the
/// message it belongs to only becomes durable when it closes. So a client
/// that reloads mid-message sees nothing of that message until `message_stop`
/// — the compacted event then arrives live and renders it whole. That window
/// is the price of not storing the deltas.
async fn publish(
    state: &AppState,
    sender: &broadcast::Sender<StreamEvent>,
    run_id: Uuid,
    seq: &mut i64,
    kind: String,
    payload: Value,
    persist: bool,
) -> ApiResult<()> {
    let at = *seq;
    *seq += 1;

    if persist {
        // Checked out per durable event rather than per streamed one: a
        // pool checkout for every delta was its own cost, next to the row.
        let mut conn = state.db.get().await?;
        diesel::insert_into(crate::schema::transcript::table)
            .values(&NewTranscript {
                id: Uuid::now_v7(),
                run_id,
                seq: at,
                kind: &kind,
                payload: payload.clone(),
                created_at: now_epoch(),
            })
            .execute(&mut conn)
            .await
            .map_err(|err| AppError::db(err, "run.insert_transcript"))?;
    }

    let _ = sender.send(StreamEvent {
        run_id,
        seq: at,
        kind,
        payload,
    });
    Ok(())
}

/// The last `seq` a run persisted, or `INPUT_SEQ` when it persisted nothing.
/// Used only as a fallback on the error path, where the run's final live `seq`
/// is unknowable (the harness never finished streaming).
async fn last_transcript_seq(state: &AppState, run_id: Uuid) -> i64 {
    let mut conn = match state.db.get().await {
        Ok(conn) => conn,
        Err(_) => return INPUT_SEQ,
    };
    let last_seq = transcript::table
        .filter(transcript::run_id.eq(run_id))
        .select(transcript::seq)
        .order(transcript::seq.desc())
        .first::<i64>(&mut conn)
        .await
        .ok();
    last_seq.map_or(INPUT_SEQ, |value| value + 1)
}

/// Persists and streams the run's terminal marker. `base_seq` is the seq the
/// marker must land after — the run's final live seq on the success path, so a
/// live client's dedupe-by-seq never drops it — and the marker occupies
/// `base_seq` itself, which `publish` then increments past.
async fn publish_terminal(
    state: &AppState,
    sender: &broadcast::Sender<StreamEvent>,
    run_id: Uuid,
    base_seq: i64,
    kind: String,
    payload: Value,
) -> ApiResult<()> {
    let mut seq = base_seq;
    publish(state, sender, run_id, &mut seq, kind, payload, true).await
}

async fn mark_run_complete(db: &DbPool, run_id: Uuid) -> ApiResult<()> {
    let mut conn = db.get().await?;
    diesel::update(run::table.filter(run::id.eq(run_id)))
        .set(run::completed_at.eq(Some(now_epoch())))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "run.mark_complete"))?;
    Ok(())
}

/// Preserves only plain assistant text when a run ends before it can commit a
/// valid exchange. Thinking and tool-call blocks are deliberately discarded:
/// the former is not reliable conversational context and the latter may hold
/// incomplete JSON that cannot be sent back to a provider as a message.
async fn record_recovery_history(db: &DbPool, run_id: Uuid, thread_id: Uuid) -> ApiResult<()> {
    use crate::models::transcript::Transcript;

    let mut conn = db.get().await?;
    let mut seed = load_seed(&mut conn, thread_id).await?;
    let rows: Vec<Transcript> = crate::schema::transcript::table
        .filter(crate::schema::transcript::run_id.eq(run_id))
        .filter(crate::schema::transcript::kind.eq(KIND_MESSAGE))
        .order(crate::schema::transcript::seq.asc())
        .select(Transcript::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "run.recovery.load_transcript"))?;

    let text = rows
        .iter()
        .filter_map(|row| {
            (row.payload.get("role").and_then(Value::as_str) == Some("assistant"))
                .then(|| row.payload.get("content"))
                .flatten()
                .and_then(Value::as_array)
        })
        .flat_map(|blocks| blocks.iter())
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text"))
                .flatten()
                .and_then(Value::as_str)
        })
        .collect::<String>();

    if text.is_empty() {
        return Ok(());
    }

    seed.messages
        .push(llm::Message::assistant(vec![llm::ContentBlock::Text {
            text,
        }]));
    let now = now_epoch();
    let lineage = serde_json::to_vec(&seed).map_err(|error| {
        tracing::error!(%run_id, error = %error, "recovery lineage failed to encode");
        AppError::Internal
    })?;

    conn.transaction::<_, AppError, _>(|conn| {
        async move {
            let request_digest = put_blob(conn, &[], now).await?;
            let lineage_digest = put_blob(conn, &lineage, now).await?;
            let exchange_id = Uuid::now_v7();

            diesel::insert_into(exchange::table)
                .values(&NewExchange {
                    id: exchange_id,
                    run_id,
                    request_blob_digest: &request_digest,
                    provider_events_digest: None,
                    canonical_blob_digest: Some(&lineage_digest),
                    usage: None,
                    outcome: Some(json!({ "type": "recovered_text" })),
                    expected_cache_tokens: 0,
                    started_at: now,
                    completed_at: Some(now),
                })
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "run.recovery.insert_exchange"))?;

            let next_seq: i32 = diesel::update(thread::table.filter(thread::id.eq(thread_id)))
                .set(thread::next_seq.eq(thread::next_seq + 1))
                .returning(thread::next_seq)
                .get_result(conn)
                .await
                .map_err(|err| AppError::db(err, "run.recovery.advance_next_seq"))?;

            diesel::insert_into(spine::table)
                .values(&NewSpine {
                    thread_id,
                    seq: i64::from(next_seq - 1),
                    exchange_id,
                    explicit_commit: false,
                    created_at: now,
                })
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "run.recovery.insert_spine"))?;

            Ok(())
        }
        .scope_boxed()
    })
    .await
}

// ---------------------------------------------------------------------------
// Persistence: the exchange log and the spine
// ---------------------------------------------------------------------------

/// One model frame, folded out of the frame log.
#[derive(Default)]
struct ModelFrame {
    request: Option<Vec<u8>>,
    events: Vec<llm::Event>,
    usage: Option<llm::Usage>,
    outcome: Option<Value>,
}

/// Writes what the frame log recorded, and advances the thread's spine.
///
/// One transaction: an exchange that exists without the spine position naming
/// it is recoverable garbage, but a spine position naming an exchange that was
/// never written is a broken lineage, and `next_seq` moving without either is
/// a hole in the chain.
async fn record_history(
    db: &DbPool,
    run_id: Uuid,
    thread_id: Uuid,
    started_at: i64,
    outcome: RunOutcome,
) -> ApiResult<bool> {
    let frames = fold_model_frames(&outcome.frames);
    if frames.is_empty() {
        return Ok(false);
    }

    let committed_lineage = match &outcome.committed_frame {
        Some(_) => Some(serde_json::to_vec(&outcome.committed).map_err(|error| {
            tracing::error!(%run_id, error = %error, "canonical lineage failed to encode");
            AppError::Internal
        })?),
        // No commit means no position to record: the harness streamed and
        // discarded, and the lineage did not move.
        None => None,
    };
    let committed_frame = outcome.committed_frame.clone();

    let mut conn = db.get().await?;
    conn.transaction::<_, AppError, _>(|conn| {
        async move {
            let now = now_epoch();
            let mut committed_exchange: Option<Uuid> = None;

            for (frame_id, frame) in &frames {
                let is_committed = committed_frame.as_deref() == Some(frame_id.as_str());

                let request_bytes = frame.request.clone().unwrap_or_default();
                let request_digest = put_blob(conn, &request_bytes, now).await?;

                let events_digest = match serde_json::to_vec(&frame.events) {
                    Ok(bytes) => Some(put_blob(conn, &bytes, now).await?),
                    Err(error) => {
                        tracing::error!(%run_id, error = %error, "provider events failed to encode");
                        None
                    }
                };

                let lineage_digest = match (is_committed, &committed_lineage) {
                    (true, Some(bytes)) => Some(put_blob(conn, bytes, now).await?),
                    _ => None,
                };

                let exchange_id = Uuid::now_v7();
                diesel::insert_into(exchange::table)
                    .values(&NewExchange {
                        id: exchange_id,
                        run_id,
                        request_blob_digest: &request_digest,
                        provider_events_digest: events_digest.as_deref(),
                        canonical_blob_digest: lineage_digest.as_deref(),
                        usage: frame.usage.map(usage_json),
                        outcome: frame.outcome.clone(),
                        // H6's cache accounting is not instrumented yet — the
                        // expectation is derivable from the request bytes
                        // retroactively, so recording zero costs history, not
                        // correctness.
                        expected_cache_tokens: 0,
                        started_at,
                        completed_at: Some(now),
                    })
                    .execute(conn)
                    .await
                    .map_err(|err| AppError::db(err, "run.insert_exchange"))?;

                if is_committed {
                    committed_exchange = Some(exchange_id);
                }
            }

            let Some(exchange_id) = committed_exchange else {
                return Ok(false);
            };

            // The allocator is the thread's, and reading it under the same
            // transaction that writes the position is what keeps two
            // concurrent runs on one thread from claiming the same `seq`.
            let next_seq: i32 = diesel::update(thread::table.filter(thread::id.eq(thread_id)))
                .set(thread::next_seq.eq(thread::next_seq + 1))
                .returning(thread::next_seq)
                .get_result(conn)
                .await
                .map_err(|err| AppError::db(err, "run.advance_next_seq"))?;

            diesel::insert_into(spine::table)
                .values(&NewSpine {
                    thread_id,
                    seq: i64::from(next_seq - 1),
                    exchange_id,
                    // The harness called `commit` — under H4's eventual
                    // auto-commit this would be `false` for the default path,
                    // and `true` only for a deliberate best-of-N selection.
                    explicit_commit: true,
                    created_at: now,
                })
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "run.insert_spine"))?;

            Ok(true)
        }
        .scope_boxed()
    })
    .await
}

/// Groups the frame log's model events by frame, in first-seen order.
///
/// Only `model` frames become exchanges: a `tool` or `harness` frame has no
/// request bytes and nothing the exchange table models.
fn fold_model_frames(events: &[CoreEvent]) -> Vec<(FrameId, ModelFrame)> {
    let mut order: Vec<FrameId> = Vec::new();
    let mut frames: HashMap<FrameId, ModelFrame> = HashMap::new();

    fn entry(frames: &mut HashMap<FrameId, ModelFrame>, order: &mut Vec<FrameId>, frame: &FrameId) {
        if !frames.contains_key(frame) {
            frames.insert(frame.clone(), ModelFrame::default());
            order.push(frame.clone());
        }
    }

    for event in events {
        match event {
            CoreEvent::FrameStart {
                frame,
                detail: FrameDetail::Model { .. },
                ..
            } => entry(&mut frames, &mut order, frame),
            CoreEvent::ModelRequest { frame, body } => {
                entry(&mut frames, &mut order, frame);
                if let Some(slot) = frames.get_mut(frame) {
                    slot.request = Some(body.clone());
                }
            }
            CoreEvent::ModelEvent { frame, event } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.events.push(event.clone());
                }
            }
            CoreEvent::ModelUsage { frame, usage } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.usage = Some(*usage);
                }
            }
            CoreEvent::FrameStop { frame, outcome } => {
                if let Some(slot) = frames.get_mut(frame) {
                    slot.outcome = Some(match outcome {
                        Outcome::Ok => json!({ "type": "ok" }),
                        Outcome::Failed { error } => json!({
                            "type": "failed",
                            "error": error,
                        }),
                    });
                }
            }
            _ => {}
        }
    }

    order
        .into_iter()
        .filter_map(|id| frames.remove(&id).map(|frame| (id, frame)))
        .collect()
}

fn usage_json(usage: llm::Usage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_input_tokens,
        "cache_write_tokens": usage.cache_creation_input_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
    })
}

/// Content-addressed insert. A digest already present is already the same
/// bytes, so a conflict is a hit, not a collision.
async fn put_blob(conn: &mut AsyncPgConnection, bytes: &[u8], now: i64) -> ApiResult<Vec<u8>> {
    let digest = Sha256::digest(bytes).to_vec();

    diesel::insert_into(blob::table)
        .values(&NewBlob {
            digest: &digest,
            data: Some(bytes),
            storage_path: None,
            byte_length: bytes.len() as i64,
            created_at: now,
        })
        .on_conflict(blob::digest)
        .do_nothing()
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "run.put_blob"))?;

    Ok(digest)
}

/// Writes the turn's input messages from [`INPUT_SEQ`] and returns them for
/// publication.
///
/// A message the *session* had to say — that an environment was added — lands
/// here alongside the user's own words rather than being folded into them or
/// hidden. H2 makes the transcript the user-facing conversation, and a note the
/// model can see and the user cannot is not part of one.
pub async fn record_input(
    conn: &mut AsyncPgConnection,
    run_id: Uuid,
    input: &[TurnMessage],
) -> ApiResult<Vec<StreamEvent>> {
    let mut events = Vec::with_capacity(input.len());

    for (offset, turn) in input.iter().enumerate() {
        let seq = INPUT_SEQ + offset as i64;
        let payload = serde_json::to_value(harness::mapping::Message::from(&turn.message))
            .map_err(|error| {
                tracing::error!(error = %error, "input message failed to encode");
                AppError::Internal
            })?;
        let kind = turn.kind;

        diesel::insert_into(crate::schema::transcript::table)
            .values(&NewTranscript {
                id: Uuid::now_v7(),
                run_id,
                seq,
                kind,
                payload: payload.clone(),
                created_at: now_epoch(),
            })
            .execute(conn)
            .await
            .map_err(|err| AppError::db(err, "run.record_input"))?;

        events.push(StreamEvent {
            run_id,
            seq,
            kind: kind.to_owned(),
            payload,
        });
    }

    Ok(events)
}
