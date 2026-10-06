//! Notices: what a plugin says between tool calls, pushed and append-only.
//!
//! One [`Notices`] queue per session. A plugin posts into it through the
//! [`SessionCtx`](crate::SessionCtx) of its current call; the run drains it at
//! the moments [`Moment`] names; a drained batch is in flight until the turn
//! carrying it is committed ([`Notices::settle`]), and goes back into the
//! queue if the run fails first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::types::{Delivery, Message, PostError, Role, WithdrawError, text_len};

/// Bytes of text in one notice.
pub const NOTICE_CAP: usize = 8 * 1024;
/// Bytes of notice text merged into one turn. Past it, the oldest bindings'
/// notices go first and the rest wait.
pub const MERGED_CAP: usize = 32 * 1024;
/// Notices one binding may have waiting.
pub const QUEUE_CAP: usize = 64;
/// Times a run resumes after `end_turn` to deliver notices.
pub const MAX_RESUMES: u32 = 3;

/// A point in a run where notices land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// Before the user's message, at run start: `on-message`, and
    /// `after-tool-or-message` that found no tool call to follow.
    Message,
    /// Right after a tool-result turn.
    ToolResult,
    /// After the model ended its turn; a non-empty batch resumes the run.
    EndTurn,
}

impl Moment {
    fn takes(self, delivery: Delivery) -> bool {
        matches!(
            (self, delivery),
            (
                Moment::Message,
                Delivery::OnMessage | Delivery::AfterToolOrMessage
            ) | (
                Moment::ToolResult,
                Delivery::AfterToolOrRun | Delivery::AfterToolOrMessage
            ) | (
                Moment::EndTurn,
                Delivery::AfterToolOrRun | Delivery::AfterRun
            )
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Queued,
    InFlight,
}

#[derive(Debug)]
struct Entry {
    id: u64,
    binding: String,
    /// The binding's creation order: notices drain bindings in this order,
    /// then in post order.
    order: i64,
    message: Message,
    delivery: Delivery,
    state: State,
}

#[derive(Debug, Default)]
struct Queue {
    next_id: u64,
    entries: Vec<Entry>,
}

/// One session's notices.
#[derive(Debug, Default)]
pub struct Notices {
    queue: Mutex<Queue>,
}

impl Notices {
    pub fn new() -> Arc<Self> {
        Arc::new(Notices::default())
    }

    /// Checks role, size, emptiness and queue space, and queues the notice.
    /// Nothing is refused after this.
    pub fn post(
        self: &Arc<Self>,
        binding: &str,
        order: i64,
        message: Message,
        delivery: Delivery,
    ) -> Result<Pending, PostError> {
        check(&message)?;
        let mut queue = self.queue.lock().expect("notice queue poisoned");
        let waiting = queue
            .entries
            .iter()
            .filter(|entry| entry.binding == binding)
            .count();
        if waiting >= QUEUE_CAP {
            return Err(PostError::QueueFull);
        }
        let id = queue.push(binding, order, message, delivery);
        Ok(Pending {
            notices: Arc::downgrade(self),
            id,
            expired: None,
        })
    }

    /// Queues a hook message (`on-changed`, `on-removed`). Hooks follow the
    /// notice rules but have no handle, and land with the run-start turn.
    pub fn post_hook(&self, binding: &str, order: i64, message: Message) -> Result<(), PostError> {
        check(&message)?;
        let mut queue = self.queue.lock().expect("notice queue poisoned");
        queue.push(binding, order, message, Delivery::OnMessage);
        Ok(())
    }

    /// Takes what lands at `moment`, merged into at most one `system` turn and
    /// one `user` turn, system first. What is taken is in flight until
    /// [`Notices::settle`].
    pub fn drain(&self, moment: Moment) -> Vec<Message> {
        let mut queue = self.queue.lock().expect("notice queue poisoned");

        let mut due: Vec<usize> = queue
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.state == State::Queued && moment.takes(entry.delivery))
            .map(|(index, _)| index)
            .collect();
        due.sort_by_key(|&index| (queue.entries[index].order, queue.entries[index].id));

        let mut system = Vec::new();
        let mut user = Vec::new();
        let mut size = 0;
        for index in due {
            let entry = &mut queue.entries[index];
            let length = text_len(&entry.message.content);
            if size + length > MERGED_CAP {
                // In order, so everything after this waits too: a later
                // binding never jumps an earlier one that did not fit.
                break;
            }
            size += length;
            entry.state = State::InFlight;
            match entry.message.role {
                Role::System => system.push(entry.message.text()),
                _ => user.push(entry.message.text()),
            }
        }

        let mut merged = Vec::new();
        if !system.is_empty() {
            merged.push(Message::system(system.join("\n\n")));
        }
        if !user.is_empty() {
            merged.push(Message::user(user.join("\n\n")));
        }
        merged
    }

    /// Ends what is in flight: delivered when the turn carrying it was
    /// committed, back in the queue when the run failed first.
    pub fn settle(&self, committed: bool) {
        let mut queue = self.queue.lock().expect("notice queue poisoned");
        if committed {
            queue.entries.retain(|entry| entry.state != State::InFlight);
        } else {
            for entry in &mut queue.entries {
                entry.state = State::Queued;
            }
        }
    }

    /// Notices waiting, of every delivery mode.
    pub fn waiting(&self) -> usize {
        self.queue
            .lock()
            .expect("notice queue poisoned")
            .entries
            .len()
    }

    fn withdraw(&self, id: u64) -> Result<(), WithdrawError> {
        let mut queue = self.queue.lock().expect("notice queue poisoned");
        match queue.entries.iter().position(|entry| entry.id == id) {
            Some(index) if queue.entries[index].state == State::Queued => {
                queue.entries.remove(index);
                Ok(())
            }
            // In flight is already in a turn the model will read, and gone is
            // delivered (or withdrawn, which a second call cannot undo).
            _ => Err(WithdrawError::AlreadyDelivered),
        }
    }
}

impl Queue {
    fn push(&mut self, binding: &str, order: i64, message: Message, delivery: Delivery) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            id,
            binding: binding.to_owned(),
            order,
            message,
            delivery,
            state: State::Queued,
        });
        id
    }
}

fn check(message: &Message) -> Result<(), PostError> {
    if message.role == Role::Assistant {
        return Err(PostError::RoleRefused);
    }
    let length = text_len(&message.content);
    if length == 0
        || message
            .content
            .iter()
            .all(|block| block.as_text().trim().is_empty())
    {
        return Err(PostError::Empty);
    }
    if length > NOTICE_CAP {
        return Err(PostError::TooLarge);
    }
    Ok(())
}

/// One run's view of its session's notices: [`Notices::drain`], plus the
/// resume budget. After [`MAX_RESUMES`] resumes, `end_turn` ends the run and
/// waiting notices stay queued for the next run's matching moment; a user
/// interrupt cancels a pending resume.
#[derive(Debug)]
pub struct RunNotices {
    notices: Arc<Notices>,
    resumes: std::sync::atomic::AtomicU32,
}

impl RunNotices {
    pub fn new(notices: Arc<Notices>) -> Self {
        RunNotices {
            notices,
            resumes: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn drain(&self, moment: Moment, interrupted: bool) -> Vec<Message> {
        if moment != Moment::EndTurn {
            return self.notices.drain(moment);
        }
        if interrupted || self.resumes.load(Ordering::Acquire) >= MAX_RESUMES {
            return Vec::new();
        }
        let batch = self.notices.drain(moment);
        if !batch.is_empty() {
            self.resumes.fetch_add(1, Ordering::AcqRel);
        }
        batch
    }

    pub fn settle(&self, committed: bool) {
        self.notices.settle(committed);
    }
}

/// The handle `post` returns. Dropping it does not withdraw.
#[derive(Debug, Clone)]
pub struct Pending {
    notices: Weak<Notices>,
    id: u64,
    /// Set for a component's handle when the call that posted it returns. A
    /// built-in's never expires.
    expired: Option<Arc<AtomicBool>>,
}

impl Pending {
    /// Removes the notice if it is still queued.
    pub fn withdraw(&self) -> Result<(), WithdrawError> {
        if self
            .expired
            .as_ref()
            .is_some_and(|expired| expired.load(Ordering::Acquire))
        {
            return Err(WithdrawError::Expired);
        }
        match self.notices.upgrade() {
            Some(notices) => notices.withdraw(self.id),
            None => Err(WithdrawError::Expired),
        }
    }

    /// Ties this handle to a call: once `expired` is set, `withdraw` fails
    /// with `expired`. For component hosts.
    pub fn expiring(mut self, expired: Arc<AtomicBool>) -> Self {
        self.expired = Some(expired);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_moment_takes_its_delivery_modes() {
        let notices = Notices::new();
        for (text, delivery) in [
            ("tool-or-run", Delivery::AfterToolOrRun),
            ("tool-or-message", Delivery::AfterToolOrMessage),
            ("run", Delivery::AfterRun),
            ("message", Delivery::OnMessage),
        ] {
            notices.post("p", 0, Message::user(text), delivery).unwrap();
        }

        assert_eq!(
            notices.drain(Moment::ToolResult),
            vec![Message::user("tool-or-run\n\ntool-or-message")]
        );
        assert_eq!(notices.drain(Moment::EndTurn), vec![Message::user("run")]);
        assert_eq!(
            notices.drain(Moment::Message),
            vec![Message::user("message")]
        );
        assert!(notices.drain(Moment::Message).is_empty());
    }

    #[test]
    fn a_batch_is_one_system_turn_then_one_user_turn_in_binding_order() {
        let notices = Notices::new();
        notices
            .post("late", 2, Message::user("b2"), Delivery::OnMessage)
            .unwrap();
        notices
            .post("early", 1, Message::system("a1"), Delivery::OnMessage)
            .unwrap();
        notices
            .post("early", 1, Message::user("a2"), Delivery::OnMessage)
            .unwrap();
        notices
            .post("late", 2, Message::system("b1"), Delivery::OnMessage)
            .unwrap();

        assert_eq!(
            notices.drain(Moment::Message),
            vec![Message::system("a1\n\nb1"), Message::user("a2\n\nb2")]
        );
    }

    #[test]
    fn post_refuses_synchronously() {
        let notices = Notices::new();
        let assistant = Message {
            role: Role::Assistant,
            content: vec![crate::Content::text("x")],
        };
        assert_eq!(
            notices
                .post("p", 0, assistant, Delivery::OnMessage)
                .unwrap_err(),
            PostError::RoleRefused
        );
        assert_eq!(
            notices
                .post("p", 0, Message::user("  "), Delivery::OnMessage)
                .unwrap_err(),
            PostError::Empty
        );
        assert_eq!(
            notices
                .post(
                    "p",
                    0,
                    Message::user("x".repeat(NOTICE_CAP + 1)),
                    Delivery::OnMessage
                )
                .unwrap_err(),
            PostError::TooLarge
        );
        for _ in 0..QUEUE_CAP {
            notices
                .post("p", 0, Message::user("x"), Delivery::OnMessage)
                .unwrap();
        }
        assert_eq!(
            notices
                .post("p", 0, Message::user("x"), Delivery::OnMessage)
                .unwrap_err(),
            PostError::QueueFull
        );
        // The cap is per binding.
        notices
            .post("q", 0, Message::user("x"), Delivery::OnMessage)
            .unwrap();
    }

    #[test]
    fn past_the_merge_cap_the_rest_waits() {
        let notices = Notices::new();
        let big = "x".repeat(NOTICE_CAP);
        for order in 0..5 {
            notices
                .post("p", order, Message::user(big.clone()), Delivery::OnMessage)
                .unwrap();
        }
        let first = notices.drain(Moment::Message);
        assert_eq!(first[0].text().len(), 4 * NOTICE_CAP + 3 * 2);
        notices.settle(true);
        assert_eq!(notices.waiting(), 1);
    }

    #[test]
    fn withdraw_works_until_delivery_and_a_failed_run_requeues() {
        let notices = Notices::new();
        let offline = notices
            .post("env", 0, Message::user("offline"), Delivery::OnMessage)
            .unwrap();
        let other = notices
            .post("env", 0, Message::user("other"), Delivery::OnMessage)
            .unwrap();
        offline.withdraw().unwrap();

        assert_eq!(notices.drain(Moment::Message), vec![Message::user("other")]);
        assert_eq!(other.withdraw(), Err(WithdrawError::AlreadyDelivered));

        notices.settle(false);
        assert_eq!(notices.drain(Moment::Message), vec![Message::user("other")]);
        notices.settle(true);
        assert_eq!(notices.waiting(), 0);
        assert_eq!(other.withdraw(), Err(WithdrawError::AlreadyDelivered));
    }

    #[test]
    fn a_run_resumes_at_most_three_times() {
        let notices = Notices::new();
        let run = RunNotices::new(Arc::clone(&notices));
        for _ in 0..5 {
            notices
                .post("p", 0, Message::user("again"), Delivery::AfterRun)
                .unwrap();
            run.drain(Moment::EndTurn, false);
        }
        assert_eq!(notices.waiting(), 5);
        run.settle(true);
        assert_eq!(notices.waiting(), 2);
        assert!(run.drain(Moment::EndTurn, false).is_empty());

        let next = RunNotices::new(Arc::clone(&notices));
        assert!(next.drain(Moment::EndTurn, true).is_empty());
        assert_eq!(
            next.drain(Moment::EndTurn, false),
            vec![Message::user("again\n\nagain")]
        );
    }

    #[test]
    fn a_component_handle_expires_with_its_call() {
        let notices = Notices::new();
        let expired = Arc::new(AtomicBool::new(false));
        let pending = notices
            .post("p", 0, Message::user("x"), Delivery::OnMessage)
            .unwrap()
            .expiring(Arc::clone(&expired));
        expired.store(true, Ordering::Release);
        assert_eq!(pending.withdraw(), Err(WithdrawError::Expired));
    }
}
