//! Host-side push gating (§14, `docs/push-notifications.md`): which pushes
//! actually go to the relay.
//!
//! On top of the relay's own limits, `hive serve` drops a push to a phone
//! when:
//!
//! - the app is in the foreground on that phone: it holds an open
//!   `/v1/events` stream through the tunnel and sees the event live (the app
//!   cancels the stream when it goes to the background);
//! - the relay recently said it can't deliver to that phone (`no_push_token`,
//!   `revoked`) or asked the host to slow down (`rate_limited`);
//! - the [`PushLimiter`] says so: one push per `(phone, kind, chat)` per 10 s
//!   (the first of a burst goes, the rest are dropped), 20 per minute and
//!   500 per day per phone, and a host-wide 1/s token bucket with a burst of
//!   5 (under the relay's "pushes ≤ 1/s sustained").
//!
//! Everything here is pure and takes the time as an argument, so it is
//! tested without sleeping.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// The limiter's knobs.
#[derive(Debug, Clone, Copy)]
pub struct PushLimits {
    /// At most one push per `(pid, kind, chatId)` in this window.
    pub coalesce: Duration,
    pub per_minute: usize,
    pub per_day: usize,
    /// Host-wide token bucket: tokens per second and bucket size.
    pub host_rate: f64,
    pub host_burst: f64,
}

impl Default for PushLimits {
    fn default() -> Self {
        Self { coalesce: Duration::from_secs(10), per_minute: 20, per_day: 500, host_rate: 1.0, host_burst: 5.0 }
    }
}

/// Why a push was or wasn't sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Send,
    /// The app is open on this phone (live `/v1/events` stream).
    Foreground,
    /// The relay said this phone can't take pushes right now.
    PhoneBackoff,
    /// The relay asked the host to slow down.
    RelayBackoff,
    /// Another push of the same kind for the same chat went out < 10 s ago.
    Coalesced,
    PerMinute,
    PerDay,
    HostRate,
}

type CoalesceKey = (String, String, Option<String>);

/// Per-phone and host-wide push rate limits. [`PushLimiter::check`] only
/// records a push it lets through, so a dropped push never uses up budget.
#[derive(Debug)]
pub struct PushLimiter {
    limits: PushLimits,
    last: HashMap<CoalesceKey, Instant>,
    /// pid → send times in the last 24 h (at most `per_day` of them).
    sent: HashMap<String, VecDeque<Instant>>,
    tokens: f64,
    tokens_at: Option<Instant>,
}

const DAY: Duration = Duration::from_secs(86_400);
const MINUTE: Duration = Duration::from_secs(60);

impl Default for PushLimiter {
    fn default() -> Self {
        Self::new(PushLimits::default())
    }
}

impl PushLimiter {
    pub fn new(limits: PushLimits) -> Self {
        Self { limits, last: HashMap::new(), sent: HashMap::new(), tokens: limits.host_burst, tokens_at: None }
    }

    fn refill(&self, now: Instant) -> f64 {
        match self.tokens_at {
            Some(at) => (self.tokens + now.saturating_duration_since(at).as_secs_f64() * self.limits.host_rate).min(self.limits.host_burst),
            None => self.tokens,
        }
    }

    /// Decide on one push to `pid`; records it when the answer is `Send`.
    pub fn check(&mut self, pid: &str, kind: &str, chat: Option<&str>, now: Instant) -> Verdict {
        let l = self.limits;
        let key = (pid.to_string(), kind.to_string(), chat.map(str::to_string));
        if self.last.get(&key).is_some_and(|t| now.saturating_duration_since(*t) < l.coalesce) {
            return Verdict::Coalesced;
        }
        let sent = self.sent.entry(pid.to_string()).or_default();
        while sent.front().is_some_and(|t| now.saturating_duration_since(*t) >= DAY) {
            sent.pop_front();
        }
        if sent.len() >= l.per_day {
            return Verdict::PerDay;
        }
        if sent.iter().rev().take_while(|t| now.saturating_duration_since(**t) < MINUTE).count() >= l.per_minute {
            return Verdict::PerMinute;
        }
        let tokens = self.refill(now);
        if tokens < 1.0 {
            return Verdict::HostRate;
        }
        // Send: record it everywhere.
        self.tokens = tokens - 1.0;
        self.tokens_at = Some(now);
        self.sent.entry(pid.to_string()).or_default().push_back(now);
        if self.last.len() >= 1024 {
            self.last.retain(|_, t| now.saturating_duration_since(*t) < l.coalesce);
        }
        self.last.insert(key, now);
        Verdict::Send
    }
}

/// How long to hold pushes to a phone after the relay acks one with `code`,
/// or `None` when the code doesn't concern that phone alone.
pub fn phone_backoff(code: &str) -> Option<Duration> {
    match code {
        // The phone hasn't registered an APNs token with the relay (yet): it
        // registers one when it connects, which clears this (see
        // [`PushGate::foreground_open`]).
        "no_push_token" => Some(Duration::from_secs(600)),
        "revoked" => Some(Duration::from_secs(3600)),
        _ => None,
    }
}

/// How long to hold all pushes after `rate_limited` from the relay.
pub const RELAY_BACKOFF: Duration = Duration::from_secs(60);

/// Everything that decides whether a push to one phone goes out now.
#[derive(Debug, Default)]
pub struct PushGate {
    pub limiter: PushLimiter,
    /// pid → open `/v1/events` streams through the tunnel.
    foreground: HashMap<String, usize>,
    /// pid → hold pushes until.
    phone_hold: HashMap<String, Instant>,
    relay_hold: Option<Instant>,
}

impl PushGate {
    pub fn check(&mut self, pid: &str, kind: &str, chat: Option<&str>, now: Instant) -> Verdict {
        if self.foreground.get(pid).is_some_and(|n| *n > 0) {
            return Verdict::Foreground;
        }
        if self.relay_hold.is_some_and(|t| now < t) {
            return Verdict::RelayBackoff;
        }
        if self.phone_hold.get(pid).is_some_and(|t| now < *t) {
            return Verdict::PhoneBackoff;
        }
        self.limiter.check(pid, kind, chat, now)
    }

    /// An events stream for `pid` opened: the app is in the foreground, and
    /// it just connected (and registered its push token, if it has one).
    pub fn foreground_open(&mut self, pid: &str) {
        *self.foreground.entry(pid.to_string()).or_default() += 1;
        self.phone_hold.remove(pid);
    }

    pub fn foreground_close(&mut self, pid: &str) {
        if let Some(n) = self.foreground.get_mut(pid) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.foreground.remove(pid);
            }
        }
    }

    pub fn foreground_streams(&self, pid: &str) -> usize {
        self.foreground.get(pid).copied().unwrap_or(0)
    }

    /// The relay refused a push to `pid` with `code`.
    pub fn refused(&mut self, pid: &str, code: &str, now: Instant) {
        if code == "rate_limited" {
            self.relay_hold = Some(now + RELAY_BACKOFF);
        } else if let Some(d) = phone_backoff(code) {
            self.phone_hold.insert(pid.to_string(), now + d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(base: Instant, s: f64) -> Instant {
        base + Duration::from_secs_f64(s)
    }

    #[test]
    fn coalesces_same_kind_and_chat_but_delivers_the_first() {
        let t0 = Instant::now();
        let mut l = PushLimiter::default();
        assert_eq!(l.check("p", "message.done", Some("c"), t0), Verdict::Send);
        assert_eq!(l.check("p", "message.done", Some("c"), secs(t0, 2.0)), Verdict::Coalesced);
        assert_eq!(l.check("p", "message.done", Some("c"), secs(t0, 9.9)), Verdict::Coalesced);
        // Another chat, another kind or another phone isn't coalesced.
        assert_eq!(l.check("p", "message.done", Some("d"), secs(t0, 3.0)), Verdict::Send);
        assert_eq!(l.check("p", "mention", Some("c"), secs(t0, 4.0)), Verdict::Send);
        assert_eq!(l.check("q", "message.done", Some("c"), secs(t0, 5.0)), Verdict::Send);
        // After the window it goes again.
        assert_eq!(l.check("p", "message.done", Some("c"), secs(t0, 10.0)), Verdict::Send);
    }

    #[test]
    fn host_bucket_bursts_five_then_one_per_second() {
        let t0 = Instant::now();
        let mut l = PushLimiter::default();
        for i in 0..5 {
            assert_eq!(l.check("p", "k", Some(&i.to_string()), t0), Verdict::Send, "burst {i}");
        }
        assert_eq!(l.check("p", "k", Some("x"), t0), Verdict::HostRate);
        assert_eq!(l.check("p", "k", Some("x"), secs(t0, 0.5)), Verdict::HostRate);
        assert_eq!(l.check("p", "k", Some("x"), secs(t0, 1.0)), Verdict::Send);
        assert_eq!(l.check("p", "k", Some("y"), secs(t0, 1.2)), Verdict::HostRate);
        // A dropped push used no budget: the coalesce key "x" was recorded
        // only once it was sent.
        assert_eq!(l.check("p", "k", Some("y"), secs(t0, 2.0)), Verdict::Send);
    }

    #[test]
    fn per_phone_minute_and_day_caps() {
        let t0 = Instant::now();
        let limits = PushLimits { host_rate: 1000.0, host_burst: 1000.0, ..Default::default() };
        let mut l = PushLimiter::new(limits);
        for i in 0..20 {
            assert_eq!(l.check("p", "k", Some(&i.to_string()), secs(t0, i as f64)), Verdict::Send);
        }
        assert_eq!(l.check("p", "k", Some("21"), secs(t0, 30.0)), Verdict::PerMinute);
        assert_eq!(l.check("q", "k", Some("21"), secs(t0, 30.0)), Verdict::Send, "per phone");
        assert_eq!(l.check("p", "k", Some("21"), secs(t0, 60.0)), Verdict::Send, "the first one aged out");
        // 500 a day: spread them so the minute cap never bites.
        let mut l = PushLimiter::new(limits);
        for i in 0..500 {
            assert_eq!(l.check("p", "k", Some(&i.to_string()), secs(t0, i as f64 * 10.0)), Verdict::Send);
        }
        assert_eq!(l.check("p", "k", Some("x"), secs(t0, 5_001.0)), Verdict::PerDay);
        assert_eq!(l.check("p", "k", Some("x"), secs(t0, 86_400.0)), Verdict::Send, "a day later");
    }

    #[test]
    fn gate_skips_foreground_phones_and_honours_relay_refusals() {
        let t0 = Instant::now();
        let mut g = PushGate::default();
        g.foreground_open("p");
        g.foreground_open("p");
        assert_eq!(g.check("p", "k", None, t0), Verdict::Foreground);
        g.foreground_close("p");
        assert_eq!(g.check("p", "k", None, t0), Verdict::Foreground, "one stream still open");
        g.foreground_close("p");
        g.foreground_close("p"); // never underflows
        assert_eq!(g.foreground_streams("p"), 0);
        assert_eq!(g.check("p", "k", None, t0), Verdict::Send);

        g.refused("p", "no_push_token", t0);
        assert_eq!(g.check("p", "k2", None, secs(t0, 1.0)), Verdict::PhoneBackoff);
        assert_eq!(g.check("q", "k2", None, secs(t0, 1.0)), Verdict::Send, "other phones unaffected");
        g.foreground_open("p"); // the phone reconnected (token registered)
        g.foreground_close("p");
        assert_eq!(g.check("p", "k2", None, secs(t0, 2.0)), Verdict::Send);

        g.refused("p", "rate_limited", secs(t0, 3.0));
        assert_eq!(g.check("q", "k3", None, secs(t0, 4.0)), Verdict::RelayBackoff);
        assert_eq!(g.check("q", "k3", None, secs(t0, 63.0)), Verdict::Send);
        g.refused("p", "push_failed", secs(t0, 64.0));
        assert_eq!(g.check("p", "k4", None, secs(t0, 65.0)), Verdict::Send, "a one-off failure holds nothing");
    }
}
