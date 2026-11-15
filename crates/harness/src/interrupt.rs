//! An inbound stop, observable from inside a run.
//!
//! `abstract.md` is explicit that nothing reaches into a running loop to
//! steer it: control is exercised by subtraction of capability, and
//! "cancellation surfaces as an ordinary failure the harness has to deal
//! with." That is all this is. Raising the flag unwinds nothing — it makes
//! the next *outbound* step fail: a model stream being advanced, or a tool
//! being invoked, rejects exactly the way a dead provider connection does,
//! carrying the `cancelled` kind. What happens next is the loop's own
//! decision, which is the point: a loop that wants to commit what it has, or
//! say one last thing before it stops, still can.
//!
//! A loop waiting on something that is not an outbound step — a retry's
//! backoff, say — does not observe the flag until its next one. Dropping the
//! run ([`HarnessRun::terminate`](crate::HarnessRun::terminate)) is the
//! backstop for a loop that does not stop in time: the backstop, and not the
//! mechanism.

use tokio::sync::watch;

/// The raising half — held by whoever may stop the run. In `crates/api` that
/// is a registry the interrupt route reaches; the run itself never holds one.
#[derive(Clone, Debug)]
pub struct Interrupter(watch::Sender<bool>);

/// The observing half, handed to the run inside its
/// [`Grant`](crate::Grant).
#[derive(Clone, Debug)]
pub struct Interrupt(watch::Receiver<bool>);

/// Creates a fresh, unraised pair for one run.
pub fn interrupt() -> (Interrupter, Interrupt) {
    let (tx, rx) = watch::channel(false);
    (Interrupter(tx), Interrupt(rx))
}

impl Interrupter {
    /// Raises the flag. Idempotent, and safe to call after the run is over —
    /// a second stop for a run already stopping is a normal thing for a user
    /// to do, not an error to report.
    pub fn raise(&self) {
        let _ = self.0.send(true);
    }

    pub fn raised(&self) -> bool {
        *self.0.borrow()
    }
}

impl Interrupt {
    /// Whether a stop has been asked for. Cheap enough to check at the head
    /// of every step that would otherwise reach outward.
    pub fn raised(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the flag is raised, and never if it never is — including
    /// when the [`Interrupter`] has been dropped, which means nobody is left
    /// who could stop this run. Parking forever is correct there: this is
    /// always the losing side of a `select!`, so a future that resolved on a
    /// dropped sender would cancel the run it was meant to be watching.
    pub async fn raised_at(&self) {
        let mut receiver = self.0.clone();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raising_resolves_every_observer_including_later_ones() {
        let (interrupter, interrupt) = interrupt();
        assert!(!interrupt.raised());

        interrupter.raise();
        assert!(interrupt.raised());
        // Already raised before it was ever awaited — the flag is a state,
        // not an edge, so a run that checks late still sees it.
        interrupt.raised_at().await;
    }

    #[tokio::test]
    async fn a_dropped_interrupter_never_resolves() {
        let (interrupter, interrupt) = interrupt();
        drop(interrupter);

        let waited =
            tokio::time::timeout(std::time::Duration::from_millis(50), interrupt.raised_at()).await;
        assert!(
            waited.is_err(),
            "a dropped interrupter must not read as a stop"
        );
    }
}
