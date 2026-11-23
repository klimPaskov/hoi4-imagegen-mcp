//! Keeping the ChatGPT-login route polite: at most N requests at once on the machine, a minimum gap between the
//! requests of each slot, backoff on 429, a fall back to one request at a time after a rate limit, a step down of the
//! number in use after every sign-out, and a durable record that the server revoked the sign-in.
//!
//! Background (incident, 2026-11-02, about 14:29 to 14:40 UTC): every Codex-client OAuth grant on the maintainer's
//! account was revoked by the server while several sessions generated through the Codex client id at once. The
//! likely trigger is concurrency, so from then until 0.3.2 the route sent one request at a time. A maintainer decision
//! of 2026-11-05 (W5-8, a trial) allows up to three requests at once across the machine, configurable from 1 to 6.
//! The sign-in then held at six at once from 2026-11-21 with no revocation, so a maintainer decision of 2026-11-23
//! (D-141, 0.4.7) removes the fixed ceiling: with no setting the route sends up to ten at once, and a number above ten
//! takes effect only after the user confirms it. A sign-out no longer holds the route at one request for hours: after
//! the user signs in again it carries on, and every sign-out lowers the number in use by one, down to one. The
//! lowered number is the learned limit: it is kept in the shared state until the user resets it.
//! Every request this route makes (a generation, a token refresh, the sign-in code exchange) holds one slot.
//!
//! Files next to the tokens in the per-user data folder (see [`crate::store::CredentialStore::state_dir`]):
//! * `route.lock` (slot 0) and `route.lock.1`, `route.lock.2` and so on (slot N, created when a caller first reaches
//!   it) are never read or written. Each only
//!   carries an operating system file lock, which the system releases when the holding process dies, so a crash
//!   cannot leave a slot stuck. Slot 0 keeps the file name of 0.3.2, so an older process and this one never send at
//!   the same time through slot 0. A 0.4.6 or older process uses only slots 0 to 5 (its own ceiling was six), so it
//!   and this one never send at the same time through a shared slot, no slot is ever held twice, and the total never
//!   exceeds the largest configured number.
//! * `route-signin.lock` is held by the one process that waits for the user's automatic sign-in (D-143), so a status
//!   reader can tell whether the process that opened the page is still listening.
//! * `route-state.lock` carries a short-held lock around every read and every read-modify-write of the state file,
//!   so several slot holders never lose each other's records.
//! * `route-state.json` holds [`RouteState`]: the last request time of each slot (for pacing), the 429 backoff, the
//!   fall back to one request at a time after a rate limit, the learned limit after sign-outs, the revocation
//!   record, refused efforts, the per-slot pauses and the record of "Unable to verify model access" answers. It
//!   never contains a token, a header or a response body.
//!
//! Waiting for a slot or for the pacing gap honours the call's cancel token and uses the call's sleeper, so tests
//! never really wait.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use imagegen_core::config::EnvSource;
use imagegen_core::error::RetryClass;
use imagegen_core::hashing::{rfc3339_utc, Clock, SystemClock};
use imagegen_providers::types::{CancelToken, ErrorKind, ProviderError, Sleeper};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Smallest gap between two requests of one slot, across every process.
pub const MIN_GAP_SECS: i64 = 20;
/// Longest a caller waits for a free slot.
pub const LOCK_WAIT_MAX: Duration = Duration::from_secs(600);
/// How often a waiting caller tries the slots again.
pub const LOCK_POLL: Duration = Duration::from_millis(250);
/// First backoff after a 429. Doubles with every further 429 in a row.
pub const RATE_LIMIT_BACKOFF_BASE_SECS: i64 = 30;
/// The backoff never grows beyond this.
pub const RATE_LIMIT_BACKOFF_MAX_SECS: i64 = 300;
/// How many times one generation is retried after a 429 before the error is returned.
pub const RATE_LIMIT_MAX_RETRIES: u32 = 3;
/// Requests at once on the machine when nothing is configured (maintainer decision D-141, 2026-11-23; three until 0.4.6).
pub const DEFAULT_CONCURRENCY: usize = 10;
/// A configured number above this takes effect only after the user confirmed it (decision D-141). There is no upper limit.
pub const CONFIRMATION_ABOVE: usize = 10;
/// Environment variable that sets the number of requests at once, 1 or more. It wins over the project config.
pub const CONCURRENCY_ENV: &str = "HOI4_IMAGEGEN_CHATGPT_CONCURRENCY";
/// Environment variable that confirms a number above [`CONFIRMATION_ABOVE`] (`1`, `true` or `yes`). The project config
/// confirms it with `concurrency_confirmed = true` beside `concurrency`.
pub const CONCURRENCY_CONFIRMED_ENV: &str = "HOI4_IMAGEGEN_CHATGPT_CONCURRENCY_CONFIRMED";
/// How long the sign-in the tool opens after a revocation keeps its callback listening (decision D-143, 2026-11-23).
pub const AUTO_SIGN_IN_SECS: u64 = 3600;
/// The command that clears the learned limit, so the route uses the configured number again.
pub const RESET_COMMAND: &str = "hoi4-imagegen reset-concurrency";
/// How long the route stays at one request at a time after the last sign-out or rate limit.
pub const FALLBACK_HOLD_SECS: i64 = 6 * 3600;
/// A slot that got "Unable to verify model access" waits this long before its next request.
pub const MODEL_ACCESS_PAUSE_SECS: i64 = 60;
/// A second "Unable to verify model access" within this many seconds, anywhere on the machine, drops the route to
/// one request at a time.
pub const MODEL_ACCESS_WINDOW_SECS: i64 = 15 * 60;
/// How long that fall back lasts after the last occurrence. Shorter than [`FALLBACK_HOLD_SECS`]: the message is a
/// transient hiccup of the backend (three times in three minutes on 2026-11-05, with successes in between), not a
/// sign-out or a plan limit, so an hour of one at a time is enough to see whether it passes.
pub const MODEL_ACCESS_HOLD_SECS: i64 = 3600;
/// The backend message, lower case. Matched case-insensitively anywhere in an error event or an HTTP error body.
pub const MODEL_ACCESS_PHRASE: &str = "unable to verify model access";
/// Waits for pacing and backoff sleep in slices of this length so a cancel is noticed within a second.
const SLEEP_SLICE: Duration = Duration::from_secs(1);

const LOCK_FILE: &str = "route.lock";
const STATE_LOCK_FILE: &str = "route-state.lock";
const SIGN_IN_LOCK_FILE: &str = "route-signin.lock";
const STATE_FILE: &str = "route-state.json";
const STATE_VERSION: u32 = 2;

/// Fallback reason: the server signed the user out.
pub const FALLBACK_SIGN_OUT: &str = "sign_out";
/// Fallback reason: a 429 or the backend's usage-limit message.
pub const FALLBACK_RATE_LIMIT: &str = "rate_limit";

/// Fallback reason: the backend could not verify model access twice within [`MODEL_ACCESS_WINDOW_SECS`].
pub const FALLBACK_MODEL_ACCESS: &str = "model_access_unverified";

/// The sentence every revoked report starts with is built here, so status, doctor and generation agree.
pub const RELOGIN_HINT: &str = "run `hoi4-imagegen login` to sign in again";

/// The record that the server revoked the sign-in. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    /// When this machine first saw the revocation (RFC 3339, UTC).
    pub at: String,
    /// The same instant in epoch seconds.
    pub at_epoch: i64,
    /// A short error code: `token_revoked` when the body said so, else the body's own error code, else `http_<status>`.
    pub code: String,
    /// The HTTP status of the answer.
    pub status: u16,
    /// Which call saw it: `generation`, `token refresh`.
    pub during: String,
}

/// The 429 backoff in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// 429 answers in a row.
    pub failures: u32,
    /// No generation request before this epoch second.
    pub retry_after_epoch: i64,
}

/// The route dropped to one request at a time on this machine. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fallback {
    /// [`FALLBACK_SIGN_OUT`] or [`FALLBACK_RATE_LIMIT`].
    pub reason: String,
    /// A short code: `token_revoked`, `http_429`, `usage_limit` and so on.
    pub code: String,
    /// When the last trigger was seen (RFC 3339, UTC).
    pub at: String,
    /// The same instant in epoch seconds.
    pub at_epoch: i64,
    /// One request at a time until this epoch second; every new trigger moves it on.
    pub until_epoch: i64,
}

impl Fallback {
    /// Whether the fall back still holds at `now`. One recorded after a sign-out by a version up to 0.4.6 no longer
    /// holds: since 0.4.7 a sign-out lowers the learned limit ([`StepDown`]) instead.
    pub fn active(&self, now: i64) -> bool {
        // a trigger stamped more than one hold in the future (clock moved back a long way) is not trusted
        self.reason != FALLBACK_SIGN_OUT && now < self.until_epoch && self.at_epoch <= now + FALLBACK_HOLD_SECS
    }

    /// The warning shown with generations, errors, `doctor`, `login-status` and `hoi4.image_status`.
    pub fn message(&self) -> String {
        let (reason, new) = match self.reason.as_str() {
            FALLBACK_SIGN_OUT => ("a sign-out by the server", "sign-out or rate limit"),
            FALLBACK_RATE_LIMIT => ("a rate limit", "sign-out or rate limit"),
            FALLBACK_MODEL_ACCESS => ("the backend failing twice within 15 minutes to verify model access", "model-access error"),
            other => (other, "sign-out or rate limit"),
        };
        format!(
            "the ChatGPT-login route dropped to one request at a time on this machine after {reason} ({}) at {}; it goes back to several at a time after {} if no new {new} arrives",
            self.code,
            self.at,
            rfc3339_utc(self.until_epoch)
        )
    }
}

/// The learned limit: what the server's sign-outs lowered the number of requests at once to. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDown {
    /// The most requests at once the route uses: one less than the number in use at the last sign-out, never below
    /// one. It stays until the user resets it, and a configured number above it does not raise it.
    pub limit: u32,
    /// Sign-outs seen since the last reset.
    pub sign_outs: u32,
    /// When the last sign-out was seen (RFC 3339, UTC).
    pub last_sign_out_at: String,
    /// The same instant in epoch seconds.
    pub last_sign_out_epoch: i64,
}

/// The sign-in the tool started by itself after a revocation (decision D-143): it opens the page once per revocation
/// and waits for the user to finish it. Holds no secret, not even the address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSignIn {
    /// The revocation this sign-in answers, in epoch seconds. One revocation never starts a second sign-in.
    pub revoked_epoch: i64,
    /// The same revocation, RFC 3339.
    pub revoked_at: String,
    /// When the page was opened (epoch seconds, then RFC 3339).
    pub started_epoch: i64,
    /// See `started_epoch`.
    pub started_at: String,
    /// When the callback stops listening (epoch seconds).
    pub expires_epoch: i64,
    /// The process that opened the page and waits for the callback.
    pub owner_pid: u32,
    /// How it ended without a sign-in, when it did: `timed_out`, or `failed: <why>`. `None` while it waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Everything the route remembers between processes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RouteState {
    /// Format version (1 until 0.3.2, 2 since 0.3.3).
    #[serde(default)]
    pub version: u32,
    /// Set once the server revoked the sign-in; cleared by `login` and `logout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revoked>,
    /// Epoch second of the last request sent through any slot. Version 1 files have only this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request_epoch: Option<i64>,
    /// Epoch second of the last request of each slot, keyed by the slot number.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slot_last_request: BTreeMap<String, i64>,
    /// The backoff after a 429, if one is in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimit>,
    /// The learned limit after sign-outs. It stays until the user resets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_down: Option<StepDown>,
    /// The sign-in the tool started after the last revocation, until a sign-in clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<PendingSignIn>,
    /// One request at a time after a rate limit or a model-access error, if recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Fallback>,
    /// Reasoning efforts the backend refused recently, skipped until `until_epoch` so each image is one request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused_efforts: Vec<RefusedEffort>,
    /// Slots that wait before their next request, keyed by the slot number, until this epoch second.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slot_pause: BTreeMap<String, i64>,
    /// The backend's "Unable to verify model access" answers seen on this machine, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_access: Option<ModelAccess>,
}

/// The "Unable to verify model access" occurrences. Holds no secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAccess {
    /// Occurrences recorded on this machine.
    pub count: u32,
    /// When the last one was seen (RFC 3339, UTC).
    pub last_at: String,
    /// The same instant in epoch seconds.
    pub last_epoch: i64,
    /// The occurrence before the last one, in epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_epoch: Option<i64>,
}

/// What one "Unable to verify model access" came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAccessHit {
    /// Occurrences recorded so far, this one included.
    pub count: u32,
    /// This slot waits until this epoch second before its next request.
    pub pause_until: i64,
    /// This one followed another within [`MODEL_ACCESS_WINDOW_SECS`], so the route dropped to one request at a time.
    pub fallback: bool,
}

/// A reasoning effort the backend refused, remembered for a while.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedEffort {
    /// The effort, for example `high`.
    pub effort: String,
    /// Skipped until this epoch second; then it is tried again.
    pub until_epoch: i64,
}

impl RouteState {
    /// The sentence that tells the user the server signed them out. `None` when there is no revocation.
    pub fn revoked_message(&self) -> Option<String> {
        self.revoked.as_ref().map(|r| format!("signed out by the server (token revoked at {}, code {}): {RELOGIN_HINT}", r.at, r.code))
    }

    /// The fall back to one request at a time, while it holds.
    pub fn active_fallback(&self, now: i64) -> Option<&Fallback> {
        self.fallback.as_ref().filter(|f| f.active(now))
    }

    /// Requests allowed at once: the number in use, held to the learned limit, or 1 while a fall back holds.
    pub fn effective_concurrency(&self, configured: usize, now: i64) -> usize {
        if self.active_fallback(now).is_some() {
            1
        } else {
            self.learned_limit().map_or(configured, |l| configured.min(l)).max(1)
        }
    }

    /// The learned limit, if sign-outs lowered the number of requests at once.
    pub fn learned_limit(&self) -> Option<usize> {
        self.step_down.as_ref().map(|d| d.limit as usize)
    }

    /// Record a sign-out by the server: the learned limit becomes one less than the number in use now (`in_use` is the
    /// configured number the route uses before any limit), never below one. The same sign-out seen by several requests
    /// of one moment is recorded once by the caller (while [`RouteState::revoked`] is set).
    pub fn record_sign_out(&mut self, now: i64, in_use: usize) {
        let current = self.learned_limit().map_or(in_use, |l| in_use.min(l)).max(1);
        let limit = u32::try_from(current.saturating_sub(1).max(1)).unwrap_or(u32::MAX);
        let seen = self.step_down.as_ref().map_or(0, |d| d.sign_outs);
        self.step_down = Some(StepDown { limit, sign_outs: seen.saturating_add(1), last_sign_out_at: rfc3339_utc(now), last_sign_out_epoch: now });
    }

    /// The last request of `slot`. A version 1 file has no slot stamps, so its single stamp counts for every slot.
    pub fn slot_last(&self, slot: usize) -> Option<i64> {
        if self.slot_last_request.is_empty() {
            return self.last_request_epoch;
        }
        self.slot_last_request.get(&slot.to_string()).copied()
    }

    /// The last request through any slot.
    pub fn any_last(&self) -> Option<i64> {
        self.slot_last_request.values().copied().chain(self.last_request_epoch).max()
    }

    /// Claim the one automatic sign-in of the current revocation: true for the first caller only, and only while a
    /// revocation is recorded. A second caller, a later call of the same process and a sign-in that timed out all get
    /// false for the same revocation, so the page is never opened twice for it.
    pub fn claim_sign_in(&mut self, now: i64, pid: u32, window_secs: i64) -> bool {
        let Some(r) = &self.revoked else { return false };
        if self.sign_in.as_ref().is_some_and(|s| s.revoked_epoch == r.at_epoch) {
            return false;
        }
        self.sign_in = Some(PendingSignIn {
            revoked_epoch: r.at_epoch,
            revoked_at: r.at.clone(),
            started_epoch: now,
            started_at: rfc3339_utc(now),
            expires_epoch: now.saturating_add(window_secs),
            owner_pid: pid,
            outcome: None,
        });
        true
    }

    /// Record a sign-in that ended without the user signing in.
    pub fn end_sign_in(&mut self, outcome: &str) {
        if let Some(s) = self.sign_in.as_mut().filter(|s| s.outcome.is_none()) {
            s.outcome = Some(outcome.to_string());
        }
    }

    /// Record a sign-out or rate limit: one request at a time from now for [`FALLBACK_HOLD_SECS`].
    pub fn record_fallback(&mut self, now: i64, reason: &str, code: &str) {
        self.record_fallback_for(now, reason, code, FALLBACK_HOLD_SECS);
    }

    /// Record a fall back that lasts `hold_secs`. A fall back already in force that lasts longer is kept, so a short
    /// one never cuts a long one short.
    pub fn record_fallback_for(&mut self, now: i64, reason: &str, code: &str, hold_secs: i64) {
        let until_epoch = now + hold_secs;
        if self.active_fallback(now).is_some_and(|f| f.until_epoch > until_epoch) {
            return;
        }
        self.fallback = Some(Fallback { reason: reason.to_string(), code: code.to_string(), at: rfc3339_utc(now), at_epoch: now, until_epoch });
    }

    /// Record "Unable to verify model access" seen through `slot`: the slot pauses for [`MODEL_ACCESS_PAUSE_SECS`], and
    /// a second one within [`MODEL_ACCESS_WINDOW_SECS`] of the last, from any slot or process, drops the route to one
    /// request at a time for [`MODEL_ACCESS_HOLD_SECS`] after this occurrence.
    pub fn record_model_access(&mut self, slot: usize, now: i64) -> ModelAccessHit {
        let previous = self.model_access.as_ref().map(|m| m.last_epoch);
        let fallback = previous.is_some_and(|p| (0..=MODEL_ACCESS_WINDOW_SECS).contains(&(now - p)));
        let count = self.model_access.as_ref().map_or(0, |m| m.count).saturating_add(1);
        self.model_access = Some(ModelAccess { count, last_at: rfc3339_utc(now), last_epoch: now, previous_epoch: previous });
        let pause_until = now + MODEL_ACCESS_PAUSE_SECS;
        self.slot_pause.retain(|_, until| *until > now);
        self.slot_pause.insert(slot.to_string(), pause_until);
        if fallback {
            self.record_fallback_for(now, FALLBACK_MODEL_ACCESS, FALLBACK_MODEL_ACCESS, MODEL_ACCESS_HOLD_SECS);
        }
        ModelAccessHit { count, pause_until, fallback }
    }
}

/// A configured number of requests at once, at least 1. There is no upper limit.
pub fn clamp_concurrency(n: i64) -> usize {
    usize::try_from(n.max(1)).unwrap_or(usize::MAX)
}

/// The number of requests at once that was asked for, whether the user confirmed it, and the number the route uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrencySetting {
    /// The number asked for: the environment variable, else the project config, else [`DEFAULT_CONCURRENCY`].
    pub requested: usize,
    /// Where it came from: `environment`, `config` or `default`.
    pub source: &'static str,
    /// Whether the user confirmed a number above [`CONFIRMATION_ABOVE`].
    pub confirmed: bool,
    /// The number the route uses before any step down: `requested`, or [`CONFIRMATION_ABOVE`] when more was asked
    /// for without a confirmation.
    pub in_use: usize,
}

impl ConcurrencySetting {
    /// The sentence that says why fewer than `requested` are used, `None` when the number is used as asked.
    pub fn unconfirmed_note(&self) -> Option<String> {
        (self.in_use < self.requested).then(|| {
            format!(
                "{} requests at once were asked for, but a number above {CONFIRMATION_ABOVE} takes effect only after you confirm it \
                 (`concurrency_confirmed = true` beside `concurrency` under [providers.chatgpt_login] in .hoi4-imagegen/config.toml, \
                 or {CONCURRENCY_CONFIRMED_ENV}=1 beside {CONCURRENCY_ENV}), so the route uses {}",
                self.requested, self.in_use
            )
        })
    }
}

/// The requested number of requests at once ([`CONCURRENCY_ENV`] when it holds a whole number, else
/// `[providers.chatgpt_login] concurrency` from the project config, else [`DEFAULT_CONCURRENCY`]) and whether the user
/// confirmed it ([`CONCURRENCY_CONFIRMED_ENV`], or `concurrency_confirmed` in the project config). Above
/// [`CONFIRMATION_ABOVE`] the route uses that number only when confirmed.
pub fn concurrency_setting(env: &dyn EnvSource, config: Option<i64>, config_confirmed: Option<bool>) -> ConcurrencySetting {
    let from_env = env.get(CONCURRENCY_ENV).and_then(|v| v.trim().parse::<i64>().ok());
    let (asked, source) = match (from_env, config) {
        (Some(n), _) => (n, "environment"),
        (None, Some(n)) => (n, "config"),
        (None, None) => (DEFAULT_CONCURRENCY as i64, "default"),
    };
    let requested = clamp_concurrency(asked);
    let env_confirmed = env.get(CONCURRENCY_CONFIRMED_ENV).is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let confirmed = env_confirmed || config_confirmed == Some(true);
    let in_use = if requested > CONFIRMATION_ABOVE && !confirmed { CONFIRMATION_ABOVE } else { requested };
    ConcurrencySetting { requested, source, confirmed, in_use }
}

/// The number of requests at once the route uses before any step down, for a caller with no project config.
pub fn configured_concurrency(env: &dyn EnvSource, config: Option<i64>) -> usize {
    concurrency_setting(env, config, None).in_use
}

/// The error every call returns while a revocation is recorded. No request is made.
pub fn revoked_error(r: &Revoked) -> ProviderError {
    ProviderError::new(
        ErrorKind::Auth,
        RetryClass::RequiresAuthorization,
        format!("signed out by the server (token revoked at {}, code {}): {RELOGIN_HINT}", r.at, r.code),
    )
    .with_status(r.status)
}

/// Whether an answer means the server invalidated the sign-in: any 401, or a body that says so.
pub fn is_revocation(status: u16, body: &[u8]) -> bool {
    if status == 401 {
        return true;
    }
    let lower = String::from_utf8_lossy(body).to_ascii_lowercase();
    lower.contains("token_revoked") || lower.contains("invalidated oauth token")
}

/// Whether a backend message is the plan's usage limit ("You've hit your usage limit", `usage_limit_reached`).
pub fn is_usage_limit(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("usage limit") || lower.contains("usage_limit")
}

/// Whether a backend message is "Unable to verify model access right now", a transient answer the backend gave on
/// 2026-11-05 to some requests of parallel slots. It arrives as an error event in the stream or in an HTTP error body.
pub fn is_model_access_unverified(text: &str) -> bool {
    text.to_ascii_lowercase().contains(MODEL_ACCESS_PHRASE)
}

/// A short, safe error code from an answer body. Never the body itself.
pub fn error_code(status: u16, body: &[u8]) -> String {
    let lower = String::from_utf8_lossy(body).to_ascii_lowercase();
    if lower.contains("token_revoked") {
        return "token_revoked".into();
    }
    let from_json = serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| {
        v.pointer("/error/code").or_else(|| v.get("code")).or_else(|| v.get("error")).and_then(serde_json::Value::as_str).map(str::to_string)
    });
    match from_json {
        Some(c) if !c.is_empty() && c.len() <= 64 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.') => c,
        _ => format!("http_{status}"),
    }
}

/// The backoff after `failures` 429 answers in a row, in seconds.
pub fn backoff_secs(failures: u32) -> i64 {
    let shift = failures.saturating_sub(1).min(16);
    (RATE_LIMIT_BACKOFF_BASE_SECS.saturating_mul(1_i64 << shift)).min(RATE_LIMIT_BACKOFF_MAX_SECS)
}

/// The route's state in the shape `doctor` and `hoi4.image_status` show. Reads recorded state only.
pub fn report(state: &RouteState, setting: &ConcurrencySetting, now: i64) -> Value {
    let refused: Vec<Value> = state
        .refused_efforts
        .iter()
        .filter(|r| r.until_epoch > now)
        .map(|r| json!({"effort": r.effort, "skipped_until": rfc3339_utc(r.until_epoch), "remaining_secs": r.until_epoch - now}))
        .collect();
    let expired = state.refused_efforts.iter().filter(|r| r.until_epoch <= now).count();
    let backoff = state
        .rate_limit
        .as_ref()
        .filter(|r| r.retry_after_epoch > now)
        .map(|r| json!({"failures": r.failures, "retry_after": rfc3339_utc(r.retry_after_epoch), "wait_secs": r.retry_after_epoch - now}));
    let fallback = state.active_fallback(now).map(|f| {
        json!({
            "reason": f.reason,
            "code": f.code,
            "at": f.at,
            "until": rfc3339_utc(f.until_epoch),
            "remaining_secs": f.until_epoch - now,
            "message": f.message(),
        })
    });
    let last = state.any_last();
    let model_access = state.model_access.as_ref().map(|m| {
        let pauses: Vec<Value> = state
            .slot_pause
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(slot, until)| json!({"slot": slot.parse::<u64>().unwrap_or(0), "until": rfc3339_utc(*until), "remaining_secs": *until - now}))
            .collect();
        json!({
            "count": m.count,
            "last_at": m.last_at,
            "last_ago_secs": (now - m.last_epoch).max(0),
            "slot_pauses": pauses,
            "pause_secs": MODEL_ACCESS_PAUSE_SECS,
            "window_secs": MODEL_ACCESS_WINDOW_SECS,
            "hold_secs": MODEL_ACCESS_HOLD_SECS,
        })
    });
    let effective = state.effective_concurrency(setting.in_use, now);
    let mut reasons: Vec<String> = Vec::new();
    reasons.extend(setting.unconfirmed_note());
    if let Some(d) = state.step_down.as_ref() {
        reasons.push(format!(
            "the learned limit is {} at once, lowered by one at each of the {} sign-outs by the server (the last at {}); it stays until you run `{RESET_COMMAND}`",
            d.limit, d.sign_outs, d.last_sign_out_at
        ));
    }
    if let Some(f) = state.active_fallback(now) {
        reasons.push(f.message());
    }
    let sign_in = state.sign_in.as_ref().map(|s| {
        let (name, message) = match &s.outcome {
            None if now < s.expires_epoch => (
                "waiting_for_sign_in",
                format!(
                    "the server signed you out at {}; the sign-in page was opened at {} and the tool waits for you to finish it until {}. \
                     If the page did not open, or this process is gone, run `hoi4-imagegen login`",
                    s.revoked_at,
                    s.started_at,
                    rfc3339_utc(s.expires_epoch)
                ),
            ),
            None => ("timed_out", format!("the sign-in opened at {} was not finished within an hour: run `hoi4-imagegen login`", s.started_at)),
            Some(o) if o == "timed_out" => {
                ("timed_out", format!("the sign-in opened at {} was not finished within an hour: run `hoi4-imagegen login`", s.started_at))
            }
            Some(o) => ("failed", format!("the sign-in opened at {} did not finish ({o}): run `hoi4-imagegen login`", s.started_at)),
        };
        json!({
            "state": name,
            "message": message,
            "revoked_at": s.revoked_at,
            "started_at": s.started_at,
            "expires_at": rfc3339_utc(s.expires_epoch),
            "remaining_secs": (s.expires_epoch - now).max(0),
            "owner_pid": s.owner_pid,
        })
    });
    json!({
        "source": "route-state.json (recorded state only; no request was made)",
        "revoked": state.revoked.is_some(),
        "sign_in": sign_in,
        "concurrency": {
            "configured": setting.requested,
            "source": setting.source,
            "confirmed": setting.confirmed,
            "default": DEFAULT_CONCURRENCY,
            "needs_confirmation_above": CONFIRMATION_ABOVE,
            "effective": effective,
            "learned_limit": state.learned_limit(),
            "sign_outs_seen": state.step_down.as_ref().map_or(0, |d| d.sign_outs),
            "reason": if reasons.is_empty() { Value::Null } else { json!(reasons.join("; ")) },
            "env": CONCURRENCY_ENV,
            "confirmed_env": CONCURRENCY_CONFIRMED_ENV,
            "reset": RESET_COMMAND,
            "min_gap_secs_per_slot": MIN_GAP_SECS,
        },
        "fallback": fallback,
        "model_access_unverified": model_access,
        "last_request_at": last.map(rfc3339_utc),
        "last_request_ago_secs": last.map(|t| (now - t).max(0)),
        "refused_efforts": refused,
        "expired_refusals": expired,
        "rate_limit": backoff,
    })
}

/// The route's folder: its slot locks and its state.
#[derive(Clone)]
pub struct Route {
    dir: Option<PathBuf>,
    concurrency: usize,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Route").field("dir", &self.dir).field("concurrency", &self.concurrency).finish()
    }
}

impl Route {
    /// The route for a state folder, with [`DEFAULT_CONCURRENCY`] slots and the system clock. `None` makes every
    /// acquire fail closed.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Route { dir, concurrency: DEFAULT_CONCURRENCY, clock: Arc::new(SystemClock) }
    }

    /// The same route with `n` slots (at least one; there is no upper limit).
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// The same route judging the fall back's expiry by `clock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The configured number of slots.
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// The number of slots a new caller may use now: the configured number, or 1 while a fall back holds.
    pub fn effective_concurrency(&self) -> usize {
        self.state().effective_concurrency(self.concurrency, self.clock.now_secs())
    }

    fn dir(&self) -> Result<&Path, ProviderError> {
        self.dir.as_deref().ok_or_else(|| {
            ProviderError::new(
                ErrorKind::NotConfigured,
                RetryClass::RequiresAuthorization,
                "no per-user data folder (APPDATA, HOME or XDG_DATA_HOME) to keep the ChatGPT login's lock and state in, so no request is made",
            )
        })
    }

    /// Read the state. A missing or damaged file reads as the empty state.
    pub fn state(&self) -> RouteState {
        let Some(dir) = &self.dir else { return RouteState::default() };
        read_state_locked(dir)
    }

    /// Change the state under the state lock and save it.
    pub fn update(&self, f: impl FnOnce(&mut RouteState)) -> Result<(), String> {
        let Some(dir) = &self.dir else { return Ok(()) };
        update_state(dir, f)
    }

    /// The route's state in the shape of [`report`], with whether the process that waits for the automatic sign-in is
    /// still listening.
    pub fn report(&self, setting: &ConcurrencySetting, now: i64) -> Value {
        let mut v = report(&self.state(), setting, now);
        if v["sign_in"]["state"] == json!("waiting_for_sign_in") {
            let alive = self.sign_in_owner_alive();
            v["sign_in"]["owner_alive"] = json!(alive);
            if !alive {
                v["sign_in"]["state"] = json!("listener_stopped");
                v["sign_in"]["message"] =
                    json!("the process that opened the sign-in page has stopped, so nothing listens for it any more: run `hoi4-imagegen login`");
            }
        }
        v
    }

    /// Hold the file lock that says a process waits for the automatic sign-in. Dropping the file releases it.
    pub fn hold_sign_in_lock(&self) -> Option<File> {
        let dir = self.dir.as_deref()?;
        fs::create_dir_all(dir).ok()?;
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(SIGN_IN_LOCK_FILE)).ok()?;
        file.try_lock().ok()?;
        Some(file)
    }

    /// Whether a process holds [`Route::hold_sign_in_lock`].
    pub fn sign_in_owner_alive(&self) -> bool {
        let Some(dir) = self.dir.as_deref() else { return false };
        let Ok(file) = OpenOptions::new().read(true).write(true).open(dir.join(SIGN_IN_LOCK_FILE)) else { return false };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(TryLockError::WouldBlock) => true,
            Err(_) => false,
        }
    }

    /// Claim the automatic sign-in of the current revocation (see [`RouteState::claim_sign_in`]). True for one caller
    /// only, across every process of the machine.
    pub fn claim_sign_in(&self, now: i64, pid: u32, window_secs: i64) -> Result<bool, String> {
        let mut won = false;
        self.update(|st| won = st.claim_sign_in(now, pid, window_secs))?;
        Ok(won)
    }

    /// Clear the learned limit, so the route uses the configured number again (the user's choice to raise it).
    pub fn reset_concurrency(&self) -> Result<bool, String> {
        let mut had = false;
        self.update(|st| had = st.step_down.take().is_some())?;
        Ok(had)
    }

    /// Forget a recorded revocation and any backoff. Called by a successful `login` and by `logout`. A fall back to one
    /// request at a time after a rate limit is kept: it ends on its own [`FALLBACK_HOLD_SECS`] after the last one. The
    /// learned limit after sign-outs is kept too, until [`Route::reset_concurrency`].
    pub fn clear(&self) -> Result<(), String> {
        self.update(|st| {
            st.revoked = None;
            st.rate_limit = None;
            st.sign_in = None;
        })
    }

    /// Take a free slot, waiting politely. Honours `cancel` and gives up after [`LOCK_WAIT_MAX`].
    pub fn acquire(&self, cancel: &CancelToken, sleeper: &dyn Sleeper) -> Result<RouteGuard, ProviderError> {
        self.acquire_within(cancel, sleeper, LOCK_WAIT_MAX)
    }

    /// [`Route::acquire`] with an explicit bound (tests).
    pub fn acquire_within(&self, cancel: &CancelToken, sleeper: &dyn Sleeper, max_wait: Duration) -> Result<RouteGuard, ProviderError> {
        let dir = self.dir()?;
        fs::create_dir_all(dir)
            .map_err(|e| ProviderError::new(ErrorKind::Transport, RetryClass::Terminal, format!("cannot create {}: {e}", dir.display())))?;
        let (slot, file) = take_slot(dir, cancel, sleeper, max_wait, || self.effective_concurrency())?;
        Ok(RouteGuard { file, slot, dir: dir.to_path_buf(), concurrency: self.concurrency, clock: self.clock.clone() })
    }
}

/// The path of a slot's lock file. Slot 0 is `route.lock`, the file 0.3.2 and older lock.
pub fn slot_file(dir: &Path, slot: usize) -> PathBuf {
    if slot == 0 {
        dir.join(LOCK_FILE)
    } else {
        dir.join(format!("{LOCK_FILE}.{slot}"))
    }
}

fn open_lock(path: &Path) -> Result<File, ProviderError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| ProviderError::new(ErrorKind::Transport, RetryClass::Terminal, format!("cannot open {}: {e}", path.display())))
}

/// Try the slots below the current limit in order, first free one wins; poll until `max_wait` runs out.
fn take_slot(
    dir: &Path,
    cancel: &CancelToken,
    sleeper: &dyn Sleeper,
    max_wait: Duration,
    limit: impl Fn() -> usize,
) -> Result<(usize, File), ProviderError> {
    let polls = (max_wait.as_millis() / LOCK_POLL.as_millis().max(1)).max(1);
    let mut n = 1;
    for _ in 0..=polls {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        n = limit();
        for slot in 0..n {
            let path = slot_file(dir, slot);
            let file = open_lock(&path)?;
            match file.try_lock() {
                Ok(()) => return Ok((slot, file)),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => {
                    return Err(ProviderError::new(ErrorKind::Transport, RetryClass::Terminal, format!("cannot lock {}: {e}", path.display())));
                }
            }
        }
        sleeper.sleep(LOCK_POLL);
    }
    Err(ProviderError::new(
        ErrorKind::Transport,
        RetryClass::SafeSameInputs,
        format!(
            "every ChatGPT-login slot on this machine is in use ({n} request(s) at a time); try again shortly (the route limits requests at once so OpenAI does not revoke the sign-in)"
        ),
    ))
}

/// Records every sleep and never waits (tests).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct CountingSleeper(pub std::sync::Mutex<Vec<Duration>>);

#[cfg(test)]
impl CountingSleeper {
    pub(crate) fn total(&self) -> Duration {
        self.0.lock().unwrap().iter().sum()
    }
}

#[cfg(test)]
impl Sleeper for CountingSleeper {
    fn sleep(&self, d: Duration) {
        self.0.lock().unwrap().push(d);
    }
}

fn cancelled() -> ProviderError {
    ProviderError::new(ErrorKind::Cancelled, RetryClass::SafeSameInputs, "cancelled while waiting for the ChatGPT login")
}

/// Proof that this process holds one of the route's slots. Dropping it (or the process dying) releases the slot.
pub struct RouteGuard {
    file: File,
    slot: usize,
    dir: PathBuf,
    concurrency: usize,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for RouteGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouteGuard").field("slot", &self.slot).field("dir", &self.dir).field("concurrency", &self.concurrency).finish()
    }
}

impl RouteGuard {
    /// The slot this guard holds, 0 to 15.
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// The current state.
    pub fn state(&self) -> RouteState {
        read_state_locked(&self.dir)
    }

    /// Change the state under the state lock and save it. A save that fails is reported, never ignored: pacing and
    /// revocation depend on it.
    pub fn update(&self, f: impl FnOnce(&mut RouteState)) -> Result<(), ProviderError> {
        update_state(&self.dir, f).map_err(|e| ProviderError::new(ErrorKind::Transport, RetryClass::Terminal, e))
    }

    /// Requests allowed at once now: the configured number held to the learned limit, or 1 while a fall back holds.
    pub fn effective_concurrency(&self) -> usize {
        self.state().effective_concurrency(self.concurrency, self.clock.now_secs())
    }

    /// Record that the server revoked the sign-in. The first one since the user signed in again is a sign-out and
    /// lowers the learned limit by one. Another request of the same moment that gets the same answer is not counted
    /// again. Stores the time and a short code, never the body.
    pub fn record_revoked(&self, now_secs: i64, status: u16, body: &[u8], during: &str) -> Revoked {
        let revoked = Revoked { at: rfc3339_utc(now_secs), at_epoch: now_secs, code: error_code(status, body), status, during: during.to_string() };
        let copy = revoked.clone();
        // Even if the save fails the caller still gets the error; the next call will see the 401 again.
        let _ = self.update(|s| {
            if s.revoked.is_none() {
                s.record_sign_out(now_secs, self.concurrency);
            }
            s.revoked = Some(copy);
        });
        revoked
    }

    /// Record a 429: one more failure in the shared backoff, and one request at a time. Returns the backoff.
    pub fn record_rate_limited(&self, now_secs: i64) -> Result<RateLimit, ProviderError> {
        let mut out = RateLimit { failures: 1, retry_after_epoch: now_secs + backoff_secs(1) };
        self.update(|s| {
            let failures = s.rate_limit.as_ref().map(|r| r.failures).unwrap_or(0) + 1;
            out = RateLimit { failures, retry_after_epoch: now_secs + backoff_secs(failures) };
            s.rate_limit = Some(out.clone());
            s.record_fallback(now_secs, FALLBACK_RATE_LIMIT, "http_429");
        })?;
        Ok(out)
    }

    /// Record the backend's usage-limit message: one request at a time.
    pub fn record_usage_limit(&self, now_secs: i64) {
        // the caller returns the usage-limit error either way
        let _ = self.update(|s| s.record_fallback(now_secs, FALLBACK_RATE_LIMIT, "usage_limit"));
    }

    /// Record "Unable to verify model access" seen through this guard's slot (see [`RouteState::record_model_access`]).
    /// Even if the save fails the caller returns its error, so a failed save still gives a first-occurrence answer.
    pub fn record_model_access(&self, now_secs: i64) -> ModelAccessHit {
        let slot = self.slot;
        let mut hit = ModelAccessHit { count: 1, pause_until: now_secs + MODEL_ACCESS_PAUSE_SECS, fallback: false };
        let _ = self.update(|s| hit = s.record_model_access(slot, now_secs));
        hit
    }

    /// Stamp this slot's request time (and the machine-wide last request) before a request is sent.
    pub fn stamp_request(&self, now_secs: i64) -> Result<(), ProviderError> {
        let slot = self.slot;
        self.update(|s| {
            s.slot_last_request.insert(slot.to_string(), now_secs);
            s.last_request_epoch = Some(s.last_request_epoch.map_or(now_secs, |t| t.max(now_secs)));
        })
    }

    /// When the route has dropped below this guard's slot (a fall back to one request at a time), release this slot
    /// and wait for one under the new limit. Honours `cancel`.
    pub fn narrow(&mut self, cancel: &CancelToken, sleeper: &dyn Sleeper) -> Result<(), ProviderError> {
        self.narrow_within(cancel, sleeper, LOCK_WAIT_MAX)
    }

    /// [`RouteGuard::narrow`] with an explicit bound (tests).
    pub fn narrow_within(&mut self, cancel: &CancelToken, sleeper: &dyn Sleeper, max_wait: Duration) -> Result<(), ProviderError> {
        if self.slot < self.effective_concurrency() {
            return Ok(());
        }
        // let go first, so this caller never holds two slots
        let _ = self.file.unlock();
        let (concurrency, clock, dir) = (self.concurrency, self.clock.clone(), self.dir.clone());
        let (slot, file) =
            take_slot(&dir, cancel, sleeper, max_wait, || read_state_locked(&dir).effective_concurrency(concurrency, clock.now_secs()))?;
        self.slot = slot;
        self.file = file;
        Ok(())
    }

    /// Wait until `gap_until` (epoch seconds) has passed, in slices, honouring `cancel`. The wait is computed once
    /// from `now_secs`, so a sleeper that does not really sleep ends it at once.
    pub fn wait_until(&self, now_secs: i64, until_secs: i64, cancel: &CancelToken, sleeper: &dyn Sleeper) -> Result<(), ProviderError> {
        let mut remaining = (until_secs - now_secs).max(0) as u64;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let slice = remaining.min(SLEEP_SLICE.as_secs());
            sleeper.sleep(Duration::from_secs(slice));
            remaining -= slice;
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        Ok(())
    }

    /// The earliest second the next request of this slot may be sent: [`MIN_GAP_SECS`] after the slot's own last
    /// request, and after any shared backoff. While the route runs one request at a time (configured to 1, or a fall
    /// back), the gap counts from the last request of any slot, as before 0.3.3.
    pub fn next_allowed(&self, now_secs: i64) -> i64 {
        let st = self.state();
        let mut at = now_secs;
        let last = if st.effective_concurrency(self.concurrency, now_secs) == 1 { st.any_last() } else { st.slot_last(self.slot) };
        if let Some(last) = last {
            // a last-request time in the future (clock moved back) never holds a caller for more than one gap
            at = at.max((last + MIN_GAP_SECS).min(now_secs + MIN_GAP_SECS));
        }
        if let Some(rl) = &st.rate_limit {
            at = at.max(rl.retry_after_epoch.min(now_secs + RATE_LIMIT_BACKOFF_MAX_SECS));
        }
        if let Some(until) = st.slot_pause.get(&self.slot.to_string()) {
            // a pause stamped in the future (clock moved back) never holds a caller for more than one pause
            at = at.max((*until).min(now_secs + MODEL_ACCESS_PAUSE_SECS));
        }
        at
    }
}

/// Run `f` while holding the state lock. Without a lock file (no folder yet) `f` runs unlocked: there is nothing to
/// race with.
fn with_state_lock<T>(dir: &Path, exclusive: bool, f: impl FnOnce() -> T) -> T {
    let lock = OpenOptions::new().read(true).write(true).create(exclusive).truncate(false).open(dir.join(STATE_LOCK_FILE));
    let held = match &lock {
        Ok(file) => {
            if exclusive {
                file.lock().is_ok()
            } else {
                file.lock_shared().is_ok()
            }
        }
        Err(_) => false,
    };
    let out = f();
    if held {
        if let Ok(file) = &lock {
            let _ = file.unlock();
        }
    }
    out
}

fn read_state_locked(dir: &Path) -> RouteState {
    with_state_lock(dir, false, || read_state(&dir.join(STATE_FILE)))
}

fn update_state(dir: &Path, f: impl FnOnce(&mut RouteState)) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    with_state_lock(dir, true, || {
        let path = dir.join(STATE_FILE);
        let mut st = read_state(&path);
        f(&mut st);
        write_state(&path, &st)
    })
}

fn read_state(path: &Path) -> RouteState {
    fs::read(path).ok().and_then(|b| serde_json::from_slice::<RouteState>(&b).ok()).unwrap_or_default()
}

fn write_state(path: &Path, st: &RouteState) -> Result<(), String> {
    let mut st = st.clone();
    st.version = STATE_VERSION;
    let bytes = serde_json::to_vec_pretty(&st).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.write_all(&bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use imagegen_core::config::MapEnv;
    use imagegen_core::hashing::FixedClock;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    fn work_dir() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join(".work");
        fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new().prefix("rt").tempdir_in(base).unwrap()
    }

    fn route_at(d: &tempfile::TempDir, n: usize, now: i64) -> Route {
        Route::new(Some(d.path().to_path_buf())).with_concurrency(n).with_clock(Arc::new(FixedClock(now)))
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!([1, 2, 3, 4, 5, 6, 40].map(backoff_secs), [30, 60, 120, 240, 300, 300, 300]);
    }

    #[test]
    fn revocation_is_recognised_by_status_or_body() {
        assert!(is_revocation(401, b""));
        assert!(is_revocation(403, br#"{"error":{"code":"token_revoked"}}"#));
        assert!(is_revocation(400, b"Encountered invalidated oauth token for user"));
        assert!(!is_revocation(429, b"slow down"));
        assert!(!is_revocation(500, b"boom"));
    }

    #[test]
    fn the_usage_limit_message_is_recognised() {
        assert!(is_usage_limit("You've hit your usage limit"));
        assert!(is_usage_limit(r#"{"error":{"type":"usage_limit_reached"}}"#));
        assert!(!is_usage_limit("Invalid prompt."));
    }

    #[test]
    fn the_error_code_is_short_and_never_the_body() {
        assert_eq!(error_code(401, b"Encountered invalidated oauth token for user token_revoked"), "token_revoked");
        assert_eq!(error_code(401, br#"{"error":{"code":"expired_token"}}"#), "expired_token");
        assert_eq!(error_code(401, br#"{"error":"invalid_grant"}"#), "invalid_grant");
        assert_eq!(error_code(401, b"access-token-secret-looking text"), "http_401");
        assert_eq!(error_code(401, br#"{"error":{"code":"has spaces and secret"}}"#), "http_401");
    }

    #[test]
    fn the_environment_variable_wins_over_the_config_and_the_least_is_one() {
        let env = |v: &str| MapEnv::from_pairs(&[(CONCURRENCY_ENV, v)]);
        assert_eq!(configured_concurrency(&MapEnv::from_pairs(&[]), None), DEFAULT_CONCURRENCY);
        assert_eq!(DEFAULT_CONCURRENCY, 10);
        assert_eq!(configured_concurrency(&env("1"), None), 1);
        assert_eq!(configured_concurrency(&env("4"), None), 4);
        assert_eq!(configured_concurrency(&env(" 6 "), None), 6);
        assert_eq!(configured_concurrency(&env("0"), None), 1);
        assert_eq!(configured_concurrency(&env("-3"), None), 1);
        assert_eq!(configured_concurrency(&env("two"), None), DEFAULT_CONCURRENCY, "an unreadable value is ignored");
        assert_eq!(configured_concurrency(&env("two"), Some(2)), 2, "then the config applies");
        assert_eq!(configured_concurrency(&MapEnv::from_pairs(&[]), Some(5)), 5);
        assert_eq!(configured_concurrency(&env("2"), Some(5)), 2, "the variable wins");
        assert_eq!(Route::new(None).with_concurrency(0).concurrency(), 1);
        assert_eq!(Route::new(None).with_concurrency(99).concurrency(), 99, "the route itself has no ceiling");
    }

    #[test]
    fn state_round_trips_and_a_damaged_file_reads_as_empty() {
        let d = work_dir();
        let route = Route::new(Some(d.path().to_path_buf()));
        assert_eq!(route.state(), RouteState::default());
        let guard = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        guard.record_revoked(1_793_000_000, 401, b"token_revoked", "generation");
        drop(guard);
        let st = route.state();
        assert_eq!(st.revoked.as_ref().unwrap().code, "token_revoked");
        assert!(st.revoked_message().unwrap().contains("hoi4-imagegen login"));
        route.clear().unwrap();
        assert_eq!(route.state().revoked, None);
        fs::write(d.path().join(STATE_FILE), "garbage").unwrap();
        assert_eq!(route.state(), RouteState::default());
    }

    /// A state file written by 0.3.2 (format version 1): one machine-wide stamp, no slots, no fall back.
    #[test]
    fn a_version_1_state_file_reads_and_its_stamp_paces_every_slot() {
        let d = work_dir();
        let old = r#"{
  "version": 1,
  "revoked": {"at": "2026-11-02T14:40:00Z", "at_epoch": 1793630400, "code": "token_revoked", "status": 401, "during": "generation"},
  "last_request_epoch": 995,
  "rate_limit": {"failures": 1, "retry_after_epoch": 900},
  "refused_efforts": [{"effort": "high", "until_epoch": 5000}]
}"#;
        fs::write(d.path().join(STATE_FILE), old).unwrap();
        let route = route_at(&d, 3, 1000);
        let st = route.state();
        assert_eq!(st.version, 1);
        assert_eq!(st.revoked.as_ref().unwrap().code, "token_revoked");
        assert_eq!(st.refused_efforts.len(), 1);
        assert_eq!(st.fallback, None, "an old file holds no fall back");
        assert_eq!((st.slot_last(0), st.slot_last(2)), (Some(995), Some(995)), "the old stamp counts for every slot");
        let held: Vec<RouteGuard> = (0..3).map(|_| route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap()).collect();
        for g in &held {
            assert_eq!(g.next_allowed(1000), 1015, "slot {}: 20 s after the old stamp", g.slot());
        }
        // the first write keeps everything and moves the file to version 2
        held[1].stamp_request(1000).unwrap();
        let raw: Value = serde_json::from_slice(&fs::read(d.path().join(STATE_FILE)).unwrap()).unwrap();
        assert_eq!(raw["version"], 2);
        assert_eq!(raw["revoked"]["code"], "token_revoked");
        assert_eq!(raw["slot_last_request"], json!({"1": 1000}));
        assert_eq!(raw["last_request_epoch"], 1000);
        assert_eq!(held[0].next_allowed(1000), 1000, "once slots are stamped, slot 0 has no stamp of its own");
        assert_eq!(held[1].next_allowed(1000), 1020);
    }

    #[test]
    fn slots_fill_up_to_n_and_the_next_caller_gets_the_clear_error() {
        let d = work_dir();
        let route = route_at(&d, 3, 1000);
        let held: Vec<RouteGuard> = (0..3).map(|_| route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap()).collect();
        assert_eq!(held.iter().map(RouteGuard::slot).collect::<Vec<_>>(), [0, 1, 2], "the first free slot is taken");
        assert!(d.path().join("route.lock").exists() && d.path().join("route.lock.1").exists() && d.path().join("route.lock.2").exists());
        assert!(!d.path().join("route.lock.3").exists(), "a slot above N is never opened");
        let sleeper = CountingSleeper::default();
        let e = route.acquire_within(&CancelToken::new(), &sleeper, Duration::from_secs(1)).unwrap_err();
        assert!(
            e.message.contains("every ChatGPT-login slot on this machine is in use (3 request(s) at a time); try again shortly"),
            "{}",
            e.message
        );
        assert_eq!(e.retry, RetryClass::SafeSameInputs);
        assert!(!sleeper.0.lock().unwrap().is_empty(), "it waited through the sleeper, not by spinning");
        drop(held);
        assert_eq!(route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().slot(), 0, "free again after the guards dropped");
    }

    #[test]
    fn slots_are_made_on_demand_without_a_ceiling_and_an_older_process_still_shares_the_first_six() {
        let d = work_dir();
        let route = route_at(&d, 40, 1000);
        assert_eq!(route.concurrency(), 40, "there is no ceiling");
        // a 0.4.6 process (its own ceiling was six) holds slot 5 through the same file
        let old = open_lock(&slot_file(d.path(), 5)).unwrap();
        old.try_lock().unwrap();
        let held: Vec<RouteGuard> = (0..39).map(|_| route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap()).collect();
        let slots: Vec<usize> = held.iter().map(RouteGuard::slot).collect();
        let expected: Vec<usize> = (0..40).filter(|s| *s != 5).collect();
        assert_eq!(slots, expected, "slot 5 belongs to the older process, so it is skipped");
        assert!(d.path().join("route.lock.39").exists() && !d.path().join("route.lock.40").exists(), "a slot above N is never opened");
        let e = route.acquire_within(&CancelToken::new(), &CountingSleeper::default(), Duration::from_secs(1)).unwrap_err();
        assert!(e.message.contains("(40 request(s) at a time)"), "{}", e.message);
        old.unlock().unwrap();
        assert_eq!(route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().slot(), 5, "free again once the older process lets go");
    }

    fn env(pairs: &[(&str, &str)]) -> imagegen_core::config::MapEnv {
        imagegen_core::config::MapEnv::from_pairs(pairs)
    }

    #[test]
    fn the_default_is_ten_and_more_than_ten_needs_the_users_confirmation() {
        let none = concurrency_setting(&env(&[]), None, None);
        assert_eq!((none.requested, none.in_use, none.source, none.confirmed), (10, 10, "default", false));
        assert_eq!(none.unconfirmed_note(), None);
        // a number up to ten is used as it is
        assert_eq!(concurrency_setting(&env(&[]), Some(4), None).in_use, 4);
        assert_eq!(concurrency_setting(&env(&[]), Some(10), None).in_use, 10);
        // above ten, without a confirmation: ten, with the reason
        let asked = concurrency_setting(&env(&[]), Some(20), None);
        assert_eq!((asked.requested, asked.in_use, asked.confirmed), (20, 10, false));
        let note = asked.unconfirmed_note().unwrap();
        assert!(
            note.contains("20 requests at once") && note.contains("concurrency_confirmed = true") && note.contains(CONCURRENCY_CONFIRMED_ENV),
            "{note}"
        );
        assert_eq!(concurrency_setting(&env(&[]), Some(20), Some(false)).in_use, 10);
        // confirmed in the config, or in the environment, a larger number is used (there is no ceiling)
        assert_eq!(concurrency_setting(&env(&[]), Some(20), Some(true)).in_use, 20);
        assert_eq!(concurrency_setting(&env(&[(CONCURRENCY_ENV, "40"), (CONCURRENCY_CONFIRMED_ENV, "1")]), Some(2), None).in_use, 40);
        assert_eq!(concurrency_setting(&env(&[(CONCURRENCY_ENV, "40"), (CONCURRENCY_CONFIRMED_ENV, "true")]), None, None).in_use, 40);
        assert_eq!(
            concurrency_setting(&env(&[(CONCURRENCY_ENV, "40")]), None, Some(true)).in_use,
            40,
            "the config's confirmation covers the variable"
        );
        // the variable wins over the config, a value that is not a number is ignored, and the least is one
        let from_env = concurrency_setting(&env(&[(CONCURRENCY_ENV, "3")]), Some(8), None);
        assert_eq!((from_env.in_use, from_env.source), (3, "environment"));
        assert_eq!(concurrency_setting(&env(&[(CONCURRENCY_ENV, "many")]), Some(8), None).in_use, 8);
        assert_eq!(concurrency_setting(&env(&[(CONCURRENCY_ENV, "0")]), None, None).in_use, 1);
        assert_eq!(configured_concurrency(&env(&[]), Some(30)), 10, "a caller with no project config cannot confirm");
    }

    #[test]
    fn every_sign_out_lowers_the_learned_limit_by_one_down_to_one_and_the_reset_clears_it() {
        let d = work_dir();
        let t0 = 1_800_000_000;
        let route = route_at(&d, 10, t0);
        let acquire = |route: &Route| route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        // the first sign-out: no fall back to one at a time, the number in use goes from 10 to 9 for every process
        let g = acquire(&route);
        g.record_revoked(t0, 401, b"", "generation");
        let st = route.state();
        assert!(st.active_fallback(t0).is_none() && st.fallback.is_none(), "a sign-out no longer holds the route at one request");
        assert_eq!((st.learned_limit(), st.effective_concurrency(10, t0)), (Some(9), 9));
        assert_eq!(route_at(&d, 10, t0).effective_concurrency(), 9, "another process of the machine sees it");
        // another request of the same moment gets the same 401: still one sign-out
        g.record_revoked(t0 + 1, 401, b"", "generation");
        assert_eq!(route.state().step_down.as_ref().map(|d| (d.limit, d.sign_outs)), Some((9, 1)));
        // the user signs in again: the route carries on at the learned limit
        route.clear().unwrap();
        assert_eq!((route.state().revoked.is_none(), route.effective_concurrency()), (true, 9));
        // the next sign-out, hours or weeks later, lowers it again: 8, and a new process after a restart still sees it
        g.record_revoked(t0 + 30 * 86_400, 401, b"", "token refresh");
        assert_eq!(route.state().learned_limit(), Some(8));
        route.clear().unwrap();
        assert_eq!(route_at(&d, 10, t0 + 31 * 86_400).effective_concurrency(), 8, "the learned limit survives a restart");
        // a configured number above the learned limit does not raise it, one below it is used as it is
        assert_eq!((route_at(&d, 20, t0).effective_concurrency(), route_at(&d, 3, t0).effective_concurrency()), (8, 3));
        // the number in use never goes below one
        for k in 1..=12 {
            g.record_revoked(t0 + 100 * 86_400 + k, 401, b"", "generation");
            route.clear().unwrap();
        }
        assert_eq!((route.state().learned_limit(), route.state().step_down.as_ref().map(|d| d.sign_outs)), (Some(1), Some(14)));
        assert_eq!(route_at(&d, 10, t0).effective_concurrency(), 1);
        // the report says what is in use and why, and how to reset
        let setting = concurrency_setting(&env(&[]), None, None);
        let r = report(&route.state(), &setting, t0 + 101 * 86_400);
        assert_eq!((r["concurrency"]["effective"].as_u64(), r["concurrency"]["configured"].as_u64()), (Some(1), Some(10)));
        assert_eq!((r["concurrency"]["learned_limit"].as_u64(), r["concurrency"]["sign_outs_seen"].as_u64()), (Some(1), Some(14)));
        let why = r["concurrency"]["reason"].as_str().unwrap();
        assert!(why.contains("learned limit is 1") && why.contains("14 sign-outs") && why.contains(RESET_COMMAND), "{why}");
        // the user's raise: the reset clears the learned limit, and a number above ten still needs the confirmation
        assert!(route.reset_concurrency().unwrap());
        assert_eq!(route_at(&d, 10, t0).effective_concurrency(), 10);
        assert!(!route.reset_concurrency().unwrap(), "nothing left to reset");
        assert!(report(&route.state(), &setting, t0)["concurrency"]["reason"].is_null());
        let raised = concurrency_setting(&env(&[]), Some(14), None);
        assert_eq!(route_at(&d, raised.in_use, t0).effective_concurrency(), 10, "fourteen unconfirmed is ten");
        let confirmed = concurrency_setting(&env(&[]), Some(14), Some(true));
        assert_eq!(route_at(&d, confirmed.in_use, t0).effective_concurrency(), 14, "confirmed, the raise takes effect");
        // and a sign-out from the raised number gives 13
        let g = route_at(&d, confirmed.in_use, t0).acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        g.record_revoked(t0 + 200 * 86_400, 401, b"", "generation");
        assert_eq!(route_at(&d, 14, t0).effective_concurrency(), 13);
    }

    /// ZIN's setting: `concurrency = 16` and `concurrency_confirmed = true` under `[providers.chatgpt_login]` in the
    /// project config. Sixteen are used, and each sign-out then lowers the learned limit by one: 15, 14 and so on.
    #[test]
    fn sixteen_confirmed_in_the_project_config_is_used_and_a_sign_out_gives_fifteen() {
        let setting = concurrency_setting(&env(&[]), Some(16), Some(true));
        assert_eq!((setting.requested, setting.in_use, setting.confirmed, setting.source), (16, 16, true, "config"));
        let d = work_dir();
        let t0 = 1_800_000_000;
        let route = route_at(&d, setting.in_use, t0);
        assert_eq!(route.effective_concurrency(), 16);
        let g = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        g.record_revoked(t0, 401, b"", "generation");
        route.clear().unwrap();
        assert_eq!(route_at(&d, setting.in_use, t0 + 60).effective_concurrency(), 15);
        g.record_revoked(t0 + 120, 401, b"", "generation");
        route.clear().unwrap();
        assert_eq!(route_at(&d, setting.in_use, t0 + 180).effective_concurrency(), 14);
        // the same number without the confirmation is ten, and a sign-out then gives nine
        let unconfirmed = concurrency_setting(&env(&[]), Some(16), None);
        assert_eq!(unconfirmed.in_use, 10);
        let d = work_dir();
        let route = route_at(&d, unconfirmed.in_use, t0);
        route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().record_revoked(t0, 401, b"", "generation");
        assert_eq!(route.effective_concurrency(), 9);
    }

    /// A state file as 0.4.6 wrote it after a sign-out, and one that was cleared by hand.
    fn state_of_0_4_6(revoked: bool, fallback: &str) -> String {
        let revoked = if revoked {
            r#""revoked": {"at": "2026-11-23T17:34:48Z", "at_epoch": 1795455288, "code": "token_revoked", "status": 401, "during": "generation"},"#
        } else {
            ""
        };
        format!(
            r#"{{"version": 2, {revoked} "last_request_epoch": 1795455200, "slot_last_request": {{"0": 1795455200, "5": 1795455190}}, "fallback": {fallback}}}"#
        )
    }

    #[test]
    fn a_state_file_of_0_4_6_loads_and_the_route_starts_at_the_configured_number() {
        let sign_out_fallback =
            r#"{"reason": "sign_out", "code": "token_revoked", "at": "2026-11-23T17:34:48Z", "at_epoch": 1795455288, "until_epoch": 1795476888}"#;
        let t = 1_795_455_288 + 3600;
        // a revocation record and a sign-out fall back of 0.4.6: the file loads as it is, the fall back does not hold,
        // and the sign-out it names is not counted (0.4.7 starts at the configured number)
        let d = work_dir();
        std::fs::write(d.path().join(STATE_FILE), state_of_0_4_6(true, sign_out_fallback)).unwrap();
        let route = route_at(&d, 10, t);
        let st = route.state();
        assert!(st.revoked.is_some() && st.fallback.is_some() && st.step_down.is_none(), "the file loads as it is");
        assert_eq!((route.effective_concurrency(), st.learned_limit()), (10, None));
        route.clear().unwrap();
        assert_eq!((route.state().revoked.is_none(), route.state().learned_limit(), route.effective_concurrency()), (true, None, 10));
        // the next sign-out is the first one 0.4.7 counts: 10 becomes 9
        route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().record_revoked(t + 60, 401, b"", "generation");
        assert_eq!(route_at(&d, 10, t + 60).effective_concurrency(), 9);
        // a file cleared by hand (no revocation, the fall back set to null) loads too
        let d = work_dir();
        std::fs::write(d.path().join(STATE_FILE), state_of_0_4_6(false, "null")).unwrap();
        let route = route_at(&d, 10, t);
        assert_eq!((route.state().fallback.is_none(), route.state().revoked.is_none(), route.effective_concurrency()), (true, true, 10));
        assert_eq!(route.state().slot_last(5), Some(1_795_455_190), "the rest of the file is read as written");
        // a 0.4.6 sign-out fall back with no revocation record does not hold either
        let d = work_dir();
        std::fs::write(d.path().join(STATE_FILE), state_of_0_4_6(false, sign_out_fallback)).unwrap();
        assert_eq!(route_at(&d, 10, t).effective_concurrency(), 10);
    }

    #[test]
    fn a_sign_out_fall_back_written_by_an_older_version_is_ignored() {
        let d = work_dir();
        let route = route_at(&d, 10, 1000);
        route.update(|s| s.record_fallback(1000 - 60, FALLBACK_SIGN_OUT, "token_revoked")).unwrap();
        assert!(route.state().fallback.is_some());
        assert_eq!(route.effective_concurrency(), 10, "0.4.7 does not hold the route at one request after a sign-out");
        route.update(|s| s.record_fallback(1000 - 60, FALLBACK_RATE_LIMIT, "http_429")).unwrap();
        assert_eq!(route.effective_concurrency(), 1, "a rate limit still does");
    }

    #[test]
    fn one_slot_is_exclusive_and_released_on_drop() {
        let d = work_dir();
        let route = route_at(&d, 1, 1000);
        let first = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let e = route.acquire_within(&CancelToken::new(), &CountingSleeper::default(), Duration::from_secs(1)).unwrap_err();
        assert!(e.message.contains("(1 request(s) at a time)"), "{}", e.message);
        // an older process (0.3.2) locks `route.lock`: slot 0 is the same file
        let old = open_lock(&d.path().join("route.lock")).unwrap();
        assert!(matches!(old.try_lock(), Err(TryLockError::WouldBlock)), "slot 0 and the 0.3.2 lock are one file");
        drop(first);
        route.acquire(&CancelToken::new(), &CountingSleeper::default()).expect("free again after the guard dropped");
    }

    #[test]
    fn a_waiting_caller_honours_cancel() {
        let d = work_dir();
        let route = route_at(&d, 1, 1000);
        let _held = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let e = route.acquire(&cancel, &CountingSleeper::default()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Cancelled);
    }

    /// Cancels the token after its first sleep.
    struct CancelOnSleep(CancelToken, AtomicUsize);
    impl Sleeper for CancelOnSleep {
        fn sleep(&self, _d: Duration) {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.cancel();
        }
    }

    #[test]
    fn a_cancel_during_the_wait_is_noticed() {
        let d = work_dir();
        let route = route_at(&d, 1, 1000);
        let _held = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let cancel = CancelToken::new();
        let sleeper = CancelOnSleep(cancel.clone(), AtomicUsize::new(0));
        let e = route.acquire(&cancel, &sleeper).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Cancelled);
        assert_eq!(sleeper.1.load(Ordering::SeqCst), 1);
    }

    /// `threads` callers, each with its own route handle (as separate processes have), hold a slot for a while. Returns
    /// the most slots ever held at once.
    fn hammer(n: usize, threads: usize) -> usize {
        let d = work_dir();
        let inside = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(threads));
        let mut handles = Vec::new();
        for _ in 0..threads {
            let route = route_at(&d, n, 1000);
            let (inside, max_seen, start) = (inside.clone(), max_seen.clone(), start.clone());
            handles.push(std::thread::spawn(move || {
                start.wait();
                for _ in 0..3 {
                    let guard = route.acquire(&CancelToken::new(), &imagegen_providers::types::RealSleeper).unwrap();
                    assert!(guard.slot() < n);
                    let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    // wait (up to 1 s) for the others to fill the slots, so reaching N does not depend on timing
                    for _ in 0..200 {
                        if inside.load(Ordering::SeqCst) >= n {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    max_seen.fetch_max(inside.load(Ordering::SeqCst), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    inside.fetch_sub(1, Ordering::SeqCst);
                    drop(guard);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        max_seen.load(Ordering::SeqCst)
    }

    #[test]
    fn n_threads_never_hold_more_than_n_slots_and_reach_n() {
        assert_eq!(hammer(3, 7), 3, "at most three at once, and three are reached");
    }

    #[test]
    fn one_slot_serialises_callers() {
        assert_eq!(hammer(1, 4), 1, "N = 1 is the old one-at-a-time behaviour");
    }

    /// Reaching N without timing: N callers each take a slot and meet at a barrier while holding it.
    #[test]
    fn n_callers_hold_n_slots_at_the_same_moment() {
        let d = work_dir();
        let n = 4;
        let meet = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let route = route_at(&d, n, 1000);
                let meet = meet.clone();
                std::thread::spawn(move || {
                    let g = route.acquire_within(&CancelToken::new(), &imagegen_providers::types::RealSleeper, Duration::from_secs(20)).unwrap();
                    meet.wait();
                    g.slot()
                })
            })
            .collect();
        let mut slots: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        slots.sort();
        assert_eq!(slots, [0, 1, 2, 3]);
    }

    #[test]
    fn pacing_is_per_slot_and_the_backoff_holds_every_slot() {
        let d = work_dir();
        let route = route_at(&d, 3, 1000);
        let a = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let b = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        assert_eq!((a.slot(), b.slot()), (0, 1));
        assert_eq!(a.next_allowed(1000), 1000, "nothing recorded: no wait");
        a.stamp_request(995).unwrap();
        assert_eq!(a.next_allowed(1000), 1015, "20 s after slot 0's own last request");
        assert_eq!(b.next_allowed(1000), 1000, "slot 1 is not held back by slot 0's request");
        b.stamp_request(1000).unwrap();
        assert_eq!(b.next_allowed(1000), 1020);
        assert_eq!(a.next_allowed(1000), 1015, "and slot 0 not by slot 1's");
        assert_eq!(route.state().last_request_epoch, Some(1000), "the machine-wide stamp is the latest");

        a.update(|s| s.rate_limit = Some(RateLimit { failures: 2, retry_after_epoch: 1060 })).unwrap();
        assert_eq!((a.next_allowed(1000), b.next_allowed(1000)), (1060, 1060), "the shared backoff holds every slot");
        a.update(|s| {
            s.slot_last_request.insert("0".into(), 1_000_000);
        })
        .unwrap();
        assert!(a.next_allowed(1000) <= 1060, "a stamp far in the future (clock moved back) cannot hold anyone for long");

        let sleeper = CountingSleeper::default();
        a.wait_until(1000, 1003, &CancelToken::new(), &sleeper).unwrap();
        assert_eq!(sleeper.total(), Duration::from_secs(3));
    }

    #[test]
    fn with_one_slot_the_gap_counts_from_any_slot() {
        let d = work_dir();
        let route = route_at(&d, 1, 1000);
        let g = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        g.update(|s| {
            s.slot_last_request.insert("2".into(), 990);
        })
        .unwrap();
        assert_eq!(g.next_allowed(1000), 1010, "one at a time keeps one machine-wide gap");
    }

    /// Several holders with their own handles write at once; nothing is lost, and a revocation written in the middle
    /// survives.
    #[test]
    fn the_state_file_survives_concurrent_writers() {
        let d = work_dir();
        let (threads, each) = (8, 25);
        let start = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let route = route_at(&d, 6, 1000);
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for i in 0..each {
                        route.update(|s| s.refused_efforts.push(RefusedEffort { effort: format!("t{t}-{i}"), until_epoch: 1 })).unwrap();
                        if t == 3 && i == 10 {
                            let g = route.acquire(&CancelToken::new(), &imagegen_providers::types::RealSleeper).unwrap();
                            g.record_revoked(1000, 401, b"token_revoked", "generation");
                        }
                        if t == 5 && i == 12 {
                            let g = route.acquire(&CancelToken::new(), &imagegen_providers::types::RealSleeper).unwrap();
                            g.record_rate_limited(1000).unwrap();
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let st = Route::new(Some(d.path().to_path_buf())).state();
        assert_eq!(st.refused_efforts.len(), threads * each, "no write was lost");
        assert!(st.revoked.is_some(), "the revocation survived");
        assert_eq!(st.rate_limit.as_ref().map(|r| r.failures), Some(1), "the backoff survived");
        assert!(st.fallback.is_some(), "the fall back survived");
        assert!(!d.path().join("route-state.json.tmp").exists());
    }

    #[test]
    fn a_rate_limit_drops_every_new_caller_to_one_slot_with_a_clear_warning() {
        let d = work_dir();
        let route = route_at(&d, 3, 1000);
        let first = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let mut second = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        assert_eq!(second.slot(), 1);
        let rl = second.record_rate_limited(1000).unwrap();
        assert_eq!((rl.failures, rl.retry_after_epoch), (1, 1030));

        // another process with its own handle sees the fall back
        let other = route_at(&d, 3, 1000);
        assert_eq!(other.effective_concurrency(), 1);
        let st = other.state();
        let f = st.active_fallback(1000).unwrap();
        assert_eq!((f.reason.as_str(), f.code.as_str(), f.at.as_str()), ("rate_limit", "http_429", "1970-01-01T00:16:40Z"));
        assert_eq!(
            f.message(),
            "the ChatGPT-login route dropped to one request at a time on this machine after a rate limit (http_429) at 1970-01-01T00:16:40Z; it goes back to several at a time after 1970-01-01T06:16:40Z if no new sign-out or rate limit arrives"
        );
        // slot 0 is busy, so a new caller waits rather than taking slot 2
        let e = other.acquire_within(&CancelToken::new(), &CountingSleeper::default(), Duration::from_secs(1)).unwrap_err();
        assert!(e.message.contains("(1 request(s) at a time)"), "{}", e.message);

        // a holder above the new limit gives its slot up and waits for slot 0
        let e = second.narrow_within(&CancelToken::new(), &CountingSleeper::default(), Duration::from_secs(1)).unwrap_err();
        assert!(e.message.contains("in use"), "{}", e.message);
        drop(first);
        second.narrow_within(&CancelToken::new(), &CountingSleeper::default(), Duration::from_secs(1)).unwrap();
        assert_eq!(second.slot(), 0);
        let third = route_at(&d, 3, 1000 + FALLBACK_HOLD_SECS).acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        assert_eq!(third.slot(), 1, "after the hold, slot 1 is free: the narrow released it");
    }

    #[test]
    fn a_revocation_does_not_drop_the_route_to_one_slot() {
        let d = work_dir();
        let route = route_at(&d, 3, 5000);
        route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().record_revoked(5000, 401, b"token_revoked", "generation");
        assert_eq!(route.effective_concurrency(), 2, "the sign-out lowered the learned limit from 3 to 2");
        let st = route.state();
        assert!(st.fallback.is_none() && st.active_fallback(5000).is_none());
        assert_eq!(st.revoked.as_ref().map(|r| r.code.as_str()), Some("token_revoked"), "the revocation record stays");
        // a new sign-in clears the revocation, and the route carries on at the learned limit
        route.clear().unwrap();
        assert!(route.state().revoked.is_none());
        assert_eq!(route.effective_concurrency(), 2);
    }

    #[test]
    fn the_fall_back_clears_after_the_hold_and_a_new_trigger_extends_it() {
        let d = work_dir();
        let t0 = 10_000;
        route_at(&d, 3, t0).acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().record_rate_limited(t0).unwrap();
        assert_eq!(route_at(&d, 3, t0 + FALLBACK_HOLD_SECS - 1).effective_concurrency(), 1);
        assert_eq!(route_at(&d, 3, t0 + FALLBACK_HOLD_SECS).effective_concurrency(), 3, "back to three after six hours");
        // a usage-limit message five hours in restarts the hold
        let later = t0 + 5 * 3600;
        route_at(&d, 3, later).acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap().record_usage_limit(later);
        assert_eq!(route_at(&d, 3, t0 + FALLBACK_HOLD_SECS).effective_concurrency(), 1);
        assert_eq!(route_at(&d, 3, later + FALLBACK_HOLD_SECS).effective_concurrency(), 3);
        assert_eq!(route_at(&d, 3, later).state().fallback.unwrap().code, "usage_limit");
        // a trigger stamped far in the future (clock moved back a long way) is not trusted
        route_at(&d, 3, 0).update(|s| s.record_fallback(1_000_000_000, FALLBACK_RATE_LIMIT, "http_429")).unwrap();
        assert_eq!(route_at(&d, 3, 100).effective_concurrency(), 3);
        assert_eq!(route_at(&d, 3, 1_000_000_000).effective_concurrency(), 1);
    }

    #[test]
    fn the_report_shows_the_slots_and_the_fall_back() {
        let mut st = RouteState::default();
        let three = ConcurrencySetting { requested: 3, source: "config", confirmed: false, in_use: 3 };
        let r = report(&st, &three, 1000);
        let c = &r["concurrency"];
        assert_eq!((c["configured"].as_u64(), c["effective"].as_u64(), c["learned_limit"].as_u64()), (Some(3), Some(3), None));
        assert_eq!(
            (c["default"].as_u64(), c["needs_confirmation_above"].as_u64(), c["min_gap_secs_per_slot"].as_u64()),
            (Some(10), Some(10), Some(20))
        );
        assert!(c["reason"].is_null() && r["fallback"].is_null());
        // a rate limit drops the route to one at a time
        st.record_fallback(1000, FALLBACK_RATE_LIMIT, "http_429");
        st.slot_last_request.insert("2".into(), 990);
        let r = report(&st, &three, 1000);
        assert_eq!(r["concurrency"]["effective"], 1);
        assert_eq!(r["fallback"]["reason"], "rate_limit");
        assert_eq!(r["fallback"]["remaining_secs"], FALLBACK_HOLD_SECS);
        assert!(r["fallback"]["message"].as_str().unwrap().starts_with("the ChatGPT-login route dropped to one request at a time"));
        assert!(r["concurrency"]["reason"].as_str().unwrap().contains("dropped to one request at a time"));
        assert_eq!(r["last_request_ago_secs"], 10);
        // more was asked for than is used: the report says why
        let asked = ConcurrencySetting { requested: 20, source: "environment", confirmed: false, in_use: 10 };
        let r = report(&RouteState::default(), &asked, 1000);
        assert_eq!((r["concurrency"]["configured"].as_u64(), r["concurrency"]["effective"].as_u64()), (Some(20), Some(10)));
        assert!(r["concurrency"]["reason"].as_str().unwrap().contains("concurrency_confirmed"));
    }

    #[test]
    fn the_model_access_message_is_recognised_in_any_case() {
        assert!(is_model_access_unverified("Unable to verify model access right now."));
        assert!(is_model_access_unverified(r#"{"error":{"message":"UNABLE TO VERIFY MODEL ACCESS"}}"#));
        assert!(!is_model_access_unverified("Invalid prompt."));
        assert!(!is_model_access_unverified("model access denied"));
    }

    #[test]
    fn model_access_occurrences_pause_the_slot_and_the_second_within_the_window_triggers() {
        let mut st = RouteState::default();
        let a = st.record_model_access(1, 10_000);
        assert_eq!(a, ModelAccessHit { count: 1, pause_until: 10_060, fallback: false });
        assert_eq!(st.fallback, None);
        assert_eq!(st.slot_pause.get("1"), Some(&10_060));
        // exactly 15 minutes later counts, one second more does not
        let mut at_edge = st.clone();
        assert!(at_edge.record_model_access(0, 10_000 + MODEL_ACCESS_WINDOW_SECS).fallback);
        let mut past = st.clone();
        assert!(!past.record_model_access(0, 10_000 + MODEL_ACCESS_WINDOW_SECS + 1).fallback);
        // a clock that moved back never counts as "within the window"
        let mut back = st.clone();
        assert!(!back.record_model_access(0, 9_000).fallback);
        // the second triggers, for an hour from itself
        let b = st.record_model_access(0, 10_100);
        assert!(b.fallback);
        let f = st.fallback.clone().unwrap();
        assert_eq!(
            (f.reason.as_str(), f.code.as_str(), f.until_epoch),
            (FALLBACK_MODEL_ACCESS, FALLBACK_MODEL_ACCESS, 10_100 + MODEL_ACCESS_HOLD_SECS)
        );
        assert_eq!(st.fallback.as_ref().map(|f| f.until_epoch - f.at_epoch), Some(3600), "one hour, not the six of a sign-out");
        assert_eq!(st.model_access.as_ref().map(|m| (m.count, m.previous_epoch)), Some((2, Some(10_000))));
        // a long fall back already in force is not shortened
        let mut long = RouteState::default();
        long.record_fallback(10_000, FALLBACK_RATE_LIMIT, "http_429");
        long.record_model_access(0, 10_010);
        assert!(long.record_model_access(0, 10_020).fallback);
        assert_eq!(long.fallback.unwrap().reason, FALLBACK_RATE_LIMIT);
        // and a state file from 0.3.3 without these fields still reads
        let old: RouteState = serde_json::from_str(r#"{"version":2,"slot_last_request":{"0":5}}"#).unwrap();
        assert!(old.model_access.is_none() && old.slot_pause.is_empty());
    }

    #[test]
    fn a_slot_pause_holds_only_that_slot_and_is_bounded_by_a_moved_clock() {
        let d = work_dir();
        let route = route_at(&d, 3, 1000);
        let a = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        let b = route.acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap();
        a.record_model_access(1000);
        assert_eq!((a.next_allowed(1000), b.next_allowed(1000)), (1060, 1000));
        a.update(|s| {
            s.slot_pause.insert("0".into(), 9_000_000);
        })
        .unwrap();
        assert_eq!(a.next_allowed(1000), 1060, "a pause stamped far ahead holds a caller for one pause at most");
    }

    #[test]
    fn the_report_shows_the_model_access_record_and_pauses() {
        let mut st = RouteState::default();
        assert!(report(&st, &ConcurrencySetting { requested: 3, source: "config", confirmed: false, in_use: 3 }, 1000)["model_access_unverified"]
            .is_null());
        st.record_model_access(2, 1000);
        let r = report(&st, &ConcurrencySetting { requested: 3, source: "config", confirmed: false, in_use: 3 }, 1030);
        let m = &r["model_access_unverified"];
        assert_eq!((m["count"].as_u64(), m["last_ago_secs"].as_i64()), (Some(1), Some(30)));
        assert_eq!(m["slot_pauses"], json!([{"slot": 2, "until": rfc3339_utc(1060), "remaining_secs": 30}]));
        assert_eq!((m["pause_secs"].as_i64(), m["window_secs"].as_i64(), m["hold_secs"].as_i64()), (Some(60), Some(900), Some(3600)));
        assert_eq!(
            report(&st, &ConcurrencySetting { requested: 3, source: "config", confirmed: false, in_use: 3 }, 1061)["model_access_unverified"]
                ["slot_pauses"],
            json!([]),
            "an expired pause is not listed"
        );
    }

    #[test]
    fn no_data_folder_fails_closed() {
        let e = Route::new(None).acquire(&CancelToken::new(), &CountingSleeper::default()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::NotConfigured);
    }
}
