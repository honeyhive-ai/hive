//! A single timestamp newtype used across the domain models. Swift used
//! `Date` with `.now` defaults and ISO-8601 JSON; this wraps `time`'s
//! `OffsetDateTime` and (de)serializes as RFC 3339 so the SQLite-stored JSON
//! and any IPC payloads stay human-readable.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// UTC instant, serialized as an RFC 3339 string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(pub OffsetDateTime);

impl Timestamp {
    /// Current instant in UTC. Mirrors Swift's `Date.now` defaulting.
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// The Unix epoch — a stable "distant past" sentinel.
    pub fn epoch() -> Self {
        Self(OffsetDateTime::UNIX_EPOCH)
    }

    /// Construct from Unix seconds; out-of-range values fall back to the epoch.
    pub fn from_unix_seconds(secs: i64) -> Self {
        Self(OffsetDateTime::from_unix_timestamp(secs).unwrap_or(OffsetDateTime::UNIX_EPOCH))
    }

    pub fn inner(&self) -> OffsetDateTime {
        self.0
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(value: OffsetDateTime) -> Self {
        Self(value)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = self
            .0
            .format(&Rfc3339)
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let dt = OffsetDateTime::parse(&s, &Rfc3339).map_err(serde::de::Error::custom)?;
        Ok(Self(dt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_rfc3339() {
        let ts = Timestamp::epoch();
        let json = serde_json::to_string(&ts).unwrap();
        assert_eq!(json, "\"1970-01-01T00:00:00Z\"");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, back);
    }
}
