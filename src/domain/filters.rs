use crate::domain::{HostStatus, UsageCollectionStatus, UsageSnapshot};
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fmt, time::Duration};

#[derive(Clone, Debug)]
pub struct UsageFilter {
    pub status: Option<UsageCollectionStatus>,
    pub inactive_console_for: Option<Duration>,
    pub no_users_for: Option<Duration>,
    pub now: DateTime<Utc>,
}

impl UsageFilter {
    pub fn empty(now: DateTime<Utc>) -> Self {
        Self {
            status: None,
            inactive_console_for: None,
            no_users_for: None,
            now,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.inactive_console_for.is_none() && self.no_users_for.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct HostFilter {
    pub icmp_status: Option<HostStatus>,
    pub group: Option<String>,
    pub metadata: HashMap<String, String>,
    pub usage: UsageFilter,
}

// ---------------------------------------------------------------------------
// Inactivity decision — pure domain logic, storage-agnostic.
//
// "Is this host inactive for ≥ D?" is a business question:
//   1. current state must already be inactive (status=Ok and matching count=0)
//   2. at the cutoff (`now - D`), the host must have been observed and not active
//      (a Failed observation = unknown, not inactive)
//   3. no row in the window (cutoff, now] may break the claim (active row, or
//      a Failed row = lost visibility)
//
// The SQL queries that answer (2) and (3) are an implementation detail; the
// adapter implements [`InactivityHistory`] to provide them. A second backend
// (in-memory test fake, Postgres, etc.) can drop in without re-deriving the
// semantics.
// ---------------------------------------------------------------------------

/// State observed at or before the cutoff. `None` (returned by the adapter)
/// means no observation exists yet for the host within the available history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAtCutoff {
    /// status = Failed at the cutoff → state was unknown.
    Unknown,
    /// status = Ok at the cutoff. `activity_matched` reports whether the
    /// snapshot at that point matched the predicate (i.e. host was active).
    Ok { activity_matched: bool },
}

/// Which "activity" we're looking for. Console-only matches
/// `console_users > 0`; any-user matches console OR remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityPredicate {
    ConsoleOnly,
    AnyUser,
}

impl ActivityPredicate {
    /// True if the host's CURRENT snapshot already satisfies the "is
    /// currently inactive" precondition for this predicate (status Ok and
    /// the corresponding user count is zero). Failed snapshots return false
    /// because the current state is unknown — we don't claim inactivity.
    pub fn current_state_is_inactive(self, snapshot: &UsageSnapshot) -> bool {
        if snapshot.status != UsageCollectionStatus::Ok {
            return false;
        }
        let console = snapshot.console_users.unwrap_or(0);
        let remote = snapshot.remote_users.unwrap_or(0);
        match self {
            Self::ConsoleOnly => console == 0,
            Self::AnyUser => console == 0 && remote == 0,
        }
    }
}

/// Adapter-side interface for the two SQL questions inactivity evaluation needs
/// to answer. Implementors are typically per-call wrappers that hold a
/// connection reference and the host id under inspection.
pub trait InactivityHistory {
    type Error;
    fn state_at_or_before(
        &self,
        cutoff: DateTime<Utc>,
        predicate: ActivityPredicate,
    ) -> Result<Option<StateAtCutoff>, Self::Error>;
    fn window_breaks_inactivity(
        &self,
        cutoff: DateTime<Utc>,
        predicate: ActivityPredicate,
    ) -> Result<bool, Self::Error>;
}

/// Error returned by [`evaluate_inactivity`]. Carries either a malformed
/// duration (caller's input) or a storage error from the adapter.
#[derive(Debug)]
pub enum InactivityError<E> {
    InvalidDuration(String),
    Storage(E),
}

impl<E: fmt::Display> fmt::Display for InactivityError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDuration(msg) => write!(f, "invalid duration: {msg}"),
            Self::Storage(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for InactivityError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

/// Evaluate "is this host inactive for ≥ duration?" given the current snapshot
/// and an adapter that can answer historical questions.
pub fn evaluate_inactivity<H: InactivityHistory>(
    snapshot: &UsageSnapshot,
    duration: Duration,
    now: DateTime<Utc>,
    predicate: ActivityPredicate,
    history: &H,
) -> Result<bool, InactivityError<H::Error>> {
    if !predicate.current_state_is_inactive(snapshot) {
        return Ok(false);
    }
    let cutoff = now
        - chrono::Duration::from_std(duration)
            .map_err(|err| InactivityError::InvalidDuration(err.to_string()))?;
    match history
        .state_at_or_before(cutoff, predicate)
        .map_err(InactivityError::Storage)?
    {
        None => return Ok(false),
        Some(StateAtCutoff::Unknown) => return Ok(false),
        Some(StateAtCutoff::Ok {
            activity_matched: true,
        }) => return Ok(false),
        Some(StateAtCutoff::Ok {
            activity_matched: false,
        }) => {}
    }
    if history
        .window_breaks_inactivity(cutoff, predicate)
        .map_err(InactivityError::Storage)?
    {
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubHistory {
        state: Option<StateAtCutoff>,
        breaks: bool,
    }

    impl InactivityHistory for StubHistory {
        type Error = std::convert::Infallible;
        fn state_at_or_before(
            &self,
            _: DateTime<Utc>,
            _: ActivityPredicate,
        ) -> Result<Option<StateAtCutoff>, Self::Error> {
            Ok(self.state)
        }
        fn window_breaks_inactivity(
            &self,
            _: DateTime<Utc>,
            _: ActivityPredicate,
        ) -> Result<bool, Self::Error> {
            Ok(self.breaks)
        }
    }

    fn snap_ok(console: u32, remote: u32) -> UsageSnapshot {
        UsageSnapshot::success(Utc::now(), console, remote)
    }

    #[test]
    fn current_active_state_is_not_inactive() {
        let snap = snap_ok(1, 0);
        let h = StubHistory {
            state: Some(StateAtCutoff::Ok {
                activity_matched: false,
            }),
            breaks: false,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(!r, "host with console_users=1 must not be inactive");
    }

    #[test]
    fn no_prior_observation_is_not_inactive() {
        let snap = snap_ok(0, 0);
        let h = StubHistory {
            state: None,
            breaks: false,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(!r, "no history before cutoff → cannot claim inactivity");
    }

    #[test]
    fn unknown_state_at_cutoff_is_not_inactive() {
        let snap = snap_ok(0, 0);
        let h = StubHistory {
            state: Some(StateAtCutoff::Unknown),
            breaks: false,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(!r);
    }

    #[test]
    fn active_at_cutoff_is_not_inactive() {
        let snap = snap_ok(0, 0);
        let h = StubHistory {
            state: Some(StateAtCutoff::Ok {
                activity_matched: true,
            }),
            breaks: false,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(!r);
    }

    #[test]
    fn window_break_is_not_inactive() {
        let snap = snap_ok(0, 0);
        let h = StubHistory {
            state: Some(StateAtCutoff::Ok {
                activity_matched: false,
            }),
            breaks: true,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(!r);
    }

    #[test]
    fn inactive_when_prior_ok_zero_and_no_breaks() {
        let snap = snap_ok(0, 0);
        let h = StubHistory {
            state: Some(StateAtCutoff::Ok {
                activity_matched: false,
            }),
            breaks: false,
        };
        let r = evaluate_inactivity(
            &snap,
            Duration::from_secs(60),
            Utc::now(),
            ActivityPredicate::ConsoleOnly,
            &h,
        )
        .unwrap();
        assert!(r);
    }
}
