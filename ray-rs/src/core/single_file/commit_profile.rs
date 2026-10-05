//! Where commits' time goes, stage by stage, in `bench-profile` builds.
//!
//! `start()` marks a point and `end(stage, mark)` adds the time since to the
//! stage's total; `count(stage, n)` counts events. A committer hands a stamp
//! (`stamp()`) to the thread it waits for, which adds the time since with
//! `since_stamp`. Without the `bench-profile` feature all of these compile to
//! nothing; so does `timed(stage)`, which times the rest of its scope.
//! `SingleFileDB::commit_profile_report` prints the totals, per
//! event and per commit, and `commit_profile_reset` clears them.

macro_rules! stages {
  ($($(#[$doc:meta])* $stage:ident,)*) => {
    /// A step of a commit's way through `begin`, the commit queue and its
    /// group's write and publish (see `transaction.rs`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[allow(dead_code)]
    pub(crate) enum Stage {
      $($(#[$doc])* $stage,)*
    }

    #[cfg(feature = "bench-profile")]
    const NAMES: &[&str] = &[$(stringify!($stage),)*];
  };
}

stages! {
  /// A write transaction's whole `begin`.
  Begin,
  /// The part of `begin` that waits for a publish section to end.
  BeginPublishWait,
  /// The part of `begin` that claims the writer slot.
  BeginWriterSlot,
  /// The part of `begin` that takes the checkpoint gate.
  BeginGate,
  /// The part of `begin` that registers the transaction in MVCC.
  BeginRegister,
  /// The part of `begin` after its MVCC registration.
  BeginRest,
  /// `commit_transaction` before it hands its request to the queue.
  CommitPrep,
  /// From handing the request over until its outcome comes back.
  CommitWait,
  /// A write transaction's whole commit, the auto-checkpoint check included.
  CommitTotal,
  /// A queued committer waiting for its outcome or the lead.
  FollowerWait,
  /// Queued committers that parked (events only).
  FollowerParked,
  /// Queued committers handed the lead (events only).
  FollowerLead,
  /// From handing a committer the lead until it sees it.
  HandoffLatency,
  /// From delivering an outcome until its committer sees it.
  DeliverLatency,
  /// A leader taking the queued commits.
  LeadTake,
  /// A leader waiting for the commit lock.
  CommitLockWait,
  /// Epoch fence, vector store loads, target checks.
  PreChecks,
  /// The group's MVCC conflict checks and staging.
  MvccCheck,
  /// Waiting for the pager lock.
  PagerLockWait,
  /// Appending the group's records to the WAL buffer, and sealing them.
  WalAppendSeal,
  /// Writing the sealed WAL bytes.
  WalWrite,
  /// Writing the header that names them (and in Full mode, the sync).
  HeaderWrite,
  /// The durable group's open-set and replication steps.
  Settle,
  /// Waiting for the publish lock.
  PublishLockWait,
  /// Handing the lead on.
  ReleaseLead,
  /// The group's whole publish.
  Publish,
  /// Publishing staged schema names.
  PublishSchema,
  /// Waiting to hold the delta upgradable.
  PublishDeltaWait,
  /// Committing the group in MVCC.
  PublishMvcc,
  /// Recording a commit's MVCC history (per commit).
  PublishHistory,
  /// Waiting for readers before a commit's merge (per commit).
  PublishMergeWait,
  /// A commit's vectors and delta merge (per commit).
  PublishMerge,
  /// Delivering the group's outcomes.
  Deliver,
  /// Groups written (events only).
  Groups,
  /// Commits written, over every group (events only).
  GroupCommits,
  /// The automatic checkpoint's check after a commit or rollback (and the
  /// checkpoint, if it runs one inline).
  AutoCheckpointCheck,
  /// A writer waiting for WAL segment space (`wait_for_segment_space`).
  SegmentSpaceWait,
  /// A spill of the WAL into a WAL segment (under the commit lock).
  Spill,
  /// A background checkpoint's run, from its claim to its end.
  CheckpointRun,
  /// A background checkpoint's cut, while it holds the commit lock.
  CheckpointCut,
  /// A background checkpoint's install, while it holds the commit lock
  /// (the replay of the commits since its first replay, and the install).
  CheckpointLocked,
  /// Writing a header page (`persist_header`).
  HeaderPageWrite,
  /// Syncing after it, when asked to.
  HeaderSync,
}

#[cfg(feature = "bench-profile")]
mod imp {
  use super::{Stage, NAMES};
  use std::sync::atomic::{AtomicU64, Ordering};
  use std::sync::OnceLock;
  use std::time::Instant;

  const STAGES: usize = NAMES.len();
  static TOTALS: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];
  static EVENTS: [AtomicU64; STAGES] = [const { AtomicU64::new(0) }; STAGES];
  static EPOCH: OnceLock<Instant> = OnceLock::new();

  pub(crate) type Mark = Instant;

  pub(crate) fn start() -> Mark {
    Instant::now()
  }

  pub(crate) fn end(stage: Stage, mark: Mark) {
    let ns = mark.elapsed().as_nanos() as u64;
    TOTALS[stage as usize].fetch_add(ns, Ordering::Relaxed);
    EVENTS[stage as usize].fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn count(stage: Stage, n: u64) {
    EVENTS[stage as usize].fetch_add(n, Ordering::Relaxed);
  }

  /// Nanoseconds since the first stamp, plus one (0 means unset).
  pub(crate) fn stamp() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
  }

  /// Add the time since `stamp`, if set, to `stage`.
  pub(crate) fn since_stamp(stage: Stage, stamp: u64) {
    if stamp != 0 {
      let ns = self::stamp().saturating_sub(stamp);
      TOTALS[stage as usize].fetch_add(ns, Ordering::Relaxed);
      EVENTS[stage as usize].fetch_add(1, Ordering::Relaxed);
    }
  }

  /// Times what follows to the end of its scope (when dropped).
  pub(crate) struct Timed(Stage, Mark);

  impl Drop for Timed {
    fn drop(&mut self) {
      end(self.0, self.1);
    }
  }

  pub(crate) fn timed(stage: Stage) -> Timed {
    Timed(stage, start())
  }

  pub(crate) fn reset() {
    for (total, events) in TOTALS.iter().zip(&EVENTS) {
      total.store(0, Ordering::Relaxed);
      events.store(0, Ordering::Relaxed);
    }
  }

  pub(crate) fn report() -> String {
    let load = |stage: Stage| EVENTS[stage as usize].load(Ordering::Relaxed).max(1);
    let (commits, groups) = (load(Stage::GroupCommits), load(Stage::Groups));
    let mut out = format!(
      "commit profile: {commits} commits in {groups} groups ({:.2} per group)\n{:<18} {:>10} \
       {:>12} {:>12}\n",
      commits as f64 / groups as f64,
      "stage",
      "events",
      "ns/event",
      "ns/commit"
    );
    for ((name, total), events) in NAMES.iter().zip(&TOTALS).zip(&EVENTS) {
      let (total, events) = (
        total.load(Ordering::Relaxed),
        events.load(Ordering::Relaxed),
      );
      if let Some(per_event) = total.checked_div(events) {
        out.push_str(&format!(
          "{name:<18} {events:>10} {per_event:>12} {:>12}\n",
          total / commits
        ));
      }
    }
    out
  }
}

#[cfg(not(feature = "bench-profile"))]
#[allow(dead_code)]
mod imp {
  use super::Stage;

  #[derive(Clone, Copy)]
  pub(crate) struct Mark;

  #[inline(always)]
  pub(crate) fn start() -> Mark {
    Mark
  }

  #[inline(always)]
  pub(crate) fn end(_stage: Stage, _mark: Mark) {}

  #[inline(always)]
  pub(crate) fn count(_stage: Stage, _n: u64) {}

  #[inline(always)]
  pub(crate) fn stamp() -> u64 {
    0
  }

  #[inline(always)]
  pub(crate) fn since_stamp(_stage: Stage, _stamp: u64) {}

  pub(crate) struct Timed;

  #[inline(always)]
  pub(crate) fn timed(_stage: Stage) -> Timed {
    Timed
  }
}

#[allow(unused_imports)]
pub(crate) use imp::*;
