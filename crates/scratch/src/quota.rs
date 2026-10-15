//! Per-user byte quota across every session's scratch layer.
//!
//! One counter per session, grouped by user. Virtual mode moves its session's
//! counter on every write, so growth past the limit fails the write that
//! would cross it. Compute mode cannot see writes as they happen; the sandbox
//! is started with its overlay capped at what the user has left, and the
//! counter is re-read from disk when the layer comes back at idle teardown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Bytes a user's scratches may hold in total, or no limit.
#[derive(Default)]
pub struct Quota {
    limit: Option<u64>,
    users: Mutex<HashMap<String, HashMap<String, Arc<AtomicU64>>>>,
}

/// The quota is used up. Carries what the agent is told.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("scratch storage quota exceeded: {used} of {limit} bytes used across this user's sessions")]
pub struct QuotaExceeded {
    pub used: u64,
    pub limit: u64,
}

impl Quota {
    pub fn new(limit: Option<u64>) -> Self {
        Quota {
            limit,
            users: Mutex::default(),
        }
    }

    pub fn limit(&self) -> Option<u64> {
        self.limit
    }

    /// The counter for one session, created at `bytes` or reset to it.
    pub fn track(self: &Arc<Self>, user: &str, session: &str, bytes: u64) -> Usage {
        let mut users = self.users.lock().expect("quota poisoned");
        let counter = users
            .entry(user.to_owned())
            .or_default()
            .entry(session.to_owned())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        counter.store(bytes, Ordering::Relaxed);
        Usage {
            quota: Arc::clone(self),
            user: user.to_owned(),
            counter,
        }
    }

    pub fn forget(&self, user: &str, session: &str) {
        let mut users = self.users.lock().expect("quota poisoned");
        if let Some(sessions) = users.get_mut(user) {
            sessions.remove(session);
            if sessions.is_empty() {
                users.remove(user);
            }
        }
    }

    /// Bytes used by every tracked session of `user`.
    pub fn used(&self, user: &str) -> u64 {
        let users = self.users.lock().expect("quota poisoned");
        users
            .get(user)
            .map(|sessions| sessions.values().map(|c| c.load(Ordering::Relaxed)).sum())
            .unwrap_or(0)
    }

    /// Refuses when `user` is already at or past the limit — the check before
    /// a command runs, so a refused command has not run at all.
    pub fn check(&self, user: &str) -> Result<(), QuotaExceeded> {
        let Some(limit) = self.limit else {
            return Ok(());
        };
        let used = self.used(user);
        if used >= limit {
            return Err(QuotaExceeded { used, limit });
        }
        Ok(())
    }

    /// Reserves for `usage`'s session everything its user has left, on top
    /// of what the session already holds, and returns that total: what a
    /// sandbox may grow the session to. The reservation counts as used until
    /// the session's usage is set again, so sessions promoted at the same
    /// time see each other's grants instead of each being given the whole
    /// remainder. `None` without a limit.
    pub fn reserve(&self, usage: &Usage) -> Option<u64> {
        let limit = self.limit?;
        let users = self.users.lock().expect("quota poisoned");
        let used: u64 = users
            .get(&usage.user)
            .map(|sessions| {
                sessions
                    .values()
                    .map(|counter| counter.load(Ordering::Relaxed))
                    .sum()
            })
            .unwrap_or(0);
        let own = usage.counter.load(Ordering::Relaxed);
        let grant = limit.saturating_sub(used) + own;
        usage.counter.store(grant, Ordering::Relaxed);
        Some(grant)
    }

    /// What `user` has left, or `None` without a limit.
    pub fn remaining(&self, user: &str) -> Option<u64> {
        self.limit
            .map(|limit| limit.saturating_sub(self.used(user)))
    }
}

/// One session's counter.
#[derive(Clone)]
pub struct Usage {
    quota: Arc<Quota>,
    user: String,
    counter: Arc<AtomicU64>,
}

impl Usage {
    pub fn bytes(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    pub fn set(&self, bytes: u64) {
        self.counter.store(bytes, Ordering::Relaxed);
    }

    /// Records a change from `before` to `after` bytes, refusing growth that
    /// would take the user past the limit. Shrinking is never refused.
    pub fn change(&self, before: u64, after: u64) -> Result<(), QuotaExceeded> {
        if after <= before {
            let shrink = before - after;
            let mut current = self.counter.load(Ordering::Relaxed);
            while let Err(actual) = self.counter.compare_exchange_weak(
                current,
                current.saturating_sub(shrink),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                current = actual;
            }
            return Ok(());
        }
        let growth = after - before;
        if let Some(limit) = self.quota.limit {
            let used = self.quota.used(&self.user);
            if used + growth > limit {
                return Err(QuotaExceeded { used, limit });
            }
        }
        self.counter.fetch_add(growth, Ordering::Relaxed);
        Ok(())
    }

    pub fn user(&self) -> &str {
        &self.user
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_shared_by_a_users_sessions_and_no_one_elses() {
        let quota = Arc::new(Quota::new(Some(100)));
        let a = quota.track("ann", "s1", 40);
        let b = quota.track("ann", "s2", 50);
        let other = quota.track("bob", "s3", 90);
        assert_eq!(quota.used("ann"), 90);
        assert!(a.change(40, 50).is_ok());
        assert_eq!(
            b.change(50, 51),
            Err(QuotaExceeded {
                used: 100,
                limit: 100
            })
        );
        assert!(quota.check("ann").is_err());
        assert!(other.change(90, 100).is_ok());
        assert!(a.change(50, 0).is_ok());
        assert!(quota.check("ann").is_ok());
        assert_eq!(quota.remaining("ann"), Some(50));
        quota.forget("ann", "s2");
        assert_eq!(quota.used("ann"), 0);
    }

    #[test]
    fn a_reservation_is_seen_by_the_users_other_sessions() {
        let quota = Arc::new(Quota::new(Some(100)));
        let one = quota.track("ann", "one", 10);
        let two = quota.track("ann", "two", 20);
        // Promoted together: the second sees what the first was granted.
        assert_eq!(quota.reserve(&one), Some(80));
        assert_eq!(quota.reserve(&two), Some(20));
        assert_eq!(quota.remaining("ann"), Some(0));
        // Demoting sets the real size again, releasing the rest.
        one.set(30);
        assert_eq!(quota.remaining("ann"), Some(50));
    }

    #[test]
    fn without_a_limit_nothing_is_refused() {
        let quota = Arc::new(Quota::new(None));
        let usage = quota.track("ann", "s1", 0);
        assert!(usage.change(0, u64::MAX / 2).is_ok());
        assert!(quota.check("ann").is_ok());
        assert_eq!(quota.remaining("ann"), None);
    }
}
