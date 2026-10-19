// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Process-wide call controls for the TypeSafe System One adapter.
//!
//! Two controls bound load on the provider quota:
//! - a concurrency limiter (`controls.max_concurrent_requests`) gating how many
//!   requests may be in flight at once;
//! - a best-effort token guard (`controls.max_tokens_per_hour`) fed by provider
//!   `usage` after each call completes.
//!
//! State is process-wide and shared by every caller. Limits are read per call
//! from configuration, so an operator `PUT` takes effect on the next request.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

use crate::SystemOneError;

/// Rolling token-usage window for the best-effort token guard.
const QUOTA_WINDOW: Duration = Duration::from_secs(3600);

/// RAII guard that releases a concurrency slot when dropped.
pub(crate) struct ConcurrencyPermit {
    limiter: Arc<ConcurrencyLimiter>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        self.limiter.in_flight.fetch_sub(1, Ordering::SeqCst);
        // notify_one stores a permit when no waiter is registered, so a slot
        // freed just before a waiter registers is not lost.
        self.limiter.notify.notify_one();
    }
}

#[derive(Default)]
struct ConcurrencyLimiter {
    in_flight: AtomicUsize,
    notify: Notify,
}

impl ConcurrencyLimiter {
    fn try_acquire(&self, limit: usize) -> bool {
        self.in_flight
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                (current < limit).then_some(current + 1)
            })
            .is_ok()
    }

    async fn acquire(
        limiter: Arc<Self>,
        limit: usize,
        deadline: Instant,
    ) -> Result<ConcurrencyPermit, SystemOneError> {
        loop {
            if limiter.try_acquire(limit) {
                return Ok(ConcurrencyPermit { limiter });
            }
            // Register interest, then re-check. notify_one keeps a permit for
            // a not-yet-registered waiter, so a release between the failed
            // attempt and this registration still wakes us.
            let notified = limiter.notify.notified();
            if limiter.try_acquire(limit) {
                drop(notified);
                return Ok(ConcurrencyPermit { limiter });
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(SystemOneError::CapacityExceeded);
            }
        }
    }
}

#[derive(Default)]
struct QuotaGuard {
    state: Mutex<QuotaState>,
}

#[derive(Default)]
struct QuotaState {
    records: VecDeque<(Instant, u64)>,
}

fn prune_expired(records: &mut VecDeque<(Instant, u64)>, now: Instant) {
    while records
        .front()
        .is_some_and(|(at, _)| now.duration_since(*at) >= QUOTA_WINDOW)
    {
        records.pop_front();
    }
}

impl QuotaGuard {
    fn check(&self, budget: Option<u64>) -> Result<(), SystemOneError> {
        let Some(budget) = budget else {
            return Ok(());
        };
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| SystemOneError::QuotaExceeded)?;
        prune_expired(&mut state.records, now);
        let total: u64 = state
            .records
            .iter()
            .fold(0u64, |acc, (_, tokens)| acc.saturating_add(*tokens));
        if total >= budget {
            return Err(SystemOneError::QuotaExceeded);
        }
        Ok(())
    }

    fn record(&self, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let now = Instant::now();
        if let Ok(mut state) = self.state.lock() {
            // Prune here too so a disabled guard never accumulates records.
            prune_expired(&mut state.records, now);
            state.records.push_back((now, tokens));
        }
    }
}

/// Process-wide call controls shared by every System One caller.
#[derive(Clone, Default)]
pub(crate) struct CallControls {
    concurrency: Arc<ConcurrencyLimiter>,
    quota: Arc<QuotaGuard>,
}

impl CallControls {
    /// The process-wide singleton used by the adapter.
    pub(crate) fn instance() -> Arc<Self> {
        static INSTANCE: OnceLock<Arc<CallControls>> = OnceLock::new();
        Arc::clone(INSTANCE.get_or_init(|| Arc::new(Self::default())))
    }

    /// Reserve a concurrency slot, waiting up to `deadline`.
    pub(crate) async fn acquire_concurrency(
        &self,
        limit: u32,
        deadline: Instant,
    ) -> Result<ConcurrencyPermit, SystemOneError> {
        ConcurrencyLimiter::acquire(Arc::clone(&self.concurrency), limit as usize, deadline).await
    }

    /// Fail if recorded usage in the current rolling window is at or over the budget.
    pub(crate) fn check_quota(&self, budget: Option<u64>) -> Result<(), SystemOneError> {
        self.quota.check(budget)
    }

    /// Record provider usage after a successful call. Zero tokens are not counted.
    pub(crate) fn record_usage(&self, tokens: u64) {
        self.quota.record(tokens);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn concurrency_waits_until_deadline_then_fails() {
        let controls = CallControls::default();
        let deadline = Instant::now() + Duration::from_secs(10);

        let first = controls
            .acquire_concurrency(1, deadline)
            .await
            .expect("first permit");

        let second = tokio::spawn({
            let controls = controls.clone();
            async move { controls.acquire_concurrency(1, deadline).await }
        });

        tokio::time::sleep(Duration::from_secs(11)).await;
        assert!(matches!(
            second.await.expect("spawned task"),
            Err(SystemOneError::CapacityExceeded)
        ));
        drop(first);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrency_releases_slot_when_permit_drops() {
        let controls = CallControls::default();
        let deadline = Instant::now() + Duration::from_secs(10);

        let permit = controls
            .acquire_concurrency(1, deadline)
            .await
            .expect("first permit");
        drop(permit);

        controls
            .acquire_concurrency(1, deadline)
            .await
            .expect("slot released");
    }

    #[tokio::test(start_paused = true)]
    async fn concurrency_wakes_waiter_when_permit_drops() {
        let controls = CallControls::default();
        let deadline = Instant::now() + Duration::from_secs(10);

        let first = controls
            .acquire_concurrency(1, deadline)
            .await
            .expect("first permit");

        let waiter = tokio::spawn({
            let controls = controls.clone();
            async move { controls.acquire_concurrency(1, deadline).await }
        });

        // Let the waiter register its interest, then release the slot.
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(first);

        let permit = waiter
            .await
            .expect("spawned task")
            .expect("waiter acquired the released slot");
        drop(permit);
    }

    #[test]
    fn quota_check_passes_when_disabled() {
        let controls = CallControls::default();
        assert_eq!(controls.check_quota(None), Ok(()));
    }

    #[test]
    fn quota_check_fails_at_budget() {
        let controls = CallControls::default();
        controls.record_usage(50);
        controls.record_usage(50);
        assert_eq!(
            controls.check_quota(Some(100)),
            Err(SystemOneError::QuotaExceeded)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn quota_rolls_over_after_window() {
        let controls = CallControls::default();
        controls.record_usage(50);
        assert_eq!(controls.check_quota(Some(100)), Ok(()));
        controls.record_usage(50);
        assert_eq!(
            controls.check_quota(Some(100)),
            Err(SystemOneError::QuotaExceeded)
        );

        tokio::time::sleep(Duration::from_secs(3601)).await;
        assert_eq!(controls.check_quota(Some(100)), Ok(()));
    }

    #[test]
    fn zero_tokens_do_not_consume_quota() {
        let controls = CallControls::default();
        controls.record_usage(0);
        assert_eq!(controls.check_quota(Some(1)), Ok(()));
    }

    #[tokio::test(start_paused = true)]
    async fn quota_prunes_expired_records_during_record() {
        let controls = CallControls::default();
        controls.record_usage(50);
        tokio::time::sleep(Duration::from_secs(3601)).await;
        controls.record_usage(50);
        assert_eq!(
            controls.check_quota(Some(100)),
            Ok(()),
            "the first record must be pruned while recording, leaving only 50 tokens"
        );
    }

    #[test]
    fn quota_saturates_instead_of_overflowing() {
        let controls = CallControls::default();
        controls.record_usage(u64::MAX);
        controls.record_usage(u64::MAX);
        assert_eq!(
            controls.check_quota(Some(1)),
            Err(SystemOneError::QuotaExceeded)
        );
    }

    #[test]
    fn instance_is_shared_across_callers() {
        assert!(Arc::ptr_eq(
            &CallControls::instance(),
            &CallControls::instance()
        ));
    }
}
