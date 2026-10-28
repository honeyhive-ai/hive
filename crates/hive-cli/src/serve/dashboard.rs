//! `GET /v1/dashboard` — a dashboard whose content the host defines.
//!
//! The owner describes sections of panels in `dashboard.toml` in the serve
//! data dir (or `HIVE_DASHBOARD_FILE`); the format is in `docs/dashboard.md`.
//! The host evaluates every panel itself — Prometheus instant/range queries
//! against `HIVE_PROMETHEUS_URL`, or a GET of a JSON URL on the host's network
//! plus a JSON pointer — and serves the results, so the phone never talks to
//! Prometheus or those URLs. Nothing here runs a command.
//!
//! Each panel has its own timeout and cache lifetime (`refresh`), and a panel
//! that fails carries an `error` instead of failing the response. A config
//! that doesn't parse yields an empty dashboard with a top-level `error`.
//!
//! With no config file, a host whose `HIVE_PROMETHEUS_URL` is set gets
//! [`default_config`] (the old `/v1/lab/status` content, sections that have no
//! data dropped); any other host gets an empty dashboard, which the app hides.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::lab;

/// The config file name in the serve data dir.
pub const FILE_NAME: &str = "dashboard.toml";
pub const DEFAULT_TITLE: &str = "Dashboard";
const DEFAULT_REFRESH: Duration = Duration::from_secs(15);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_REFRESH: Duration = Duration::from_secs(1);
/// A failed panel is retried at least this often, whatever its `refresh`.
const ERROR_RETRY: Duration = Duration::from_secs(5);
const MAX_SERIES: usize = 3;
const MAX_POINTS: u64 = 1_000;
const MAX_JSON_BYTES: usize = 1 << 20;

// ── Config ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DashboardConfig {
    /// Shown as the app's tab title. Default "Dashboard".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Default panel refresh (cache lifetime), e.g. "15s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    /// Default per-panel timeout, e.g. "3s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, rename = "section", skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<SectionConfig>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    pub title: String,
    /// Drop the section when none of its panels has data (e.g. an exporter
    /// that isn't running on this host).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// PromQL: show the section only while this returns data (e.g. an
    /// exporter's `up` metric).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_when: Option<String>,
    #[serde(default, rename = "panel", skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<PanelConfig>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One number, optionally with a sparkline.
    #[default]
    Stat,
    /// One to three series over time.
    Timeseries,
    /// One number on a min…max scale.
    Gauge,
    /// Label/value rows drawn as bars.
    Bar,
    /// Label/value rows.
    List,
    /// A line of text (a JSON string, a number, or the static `text`).
    Text,
    /// Up/down rows.
    Status,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Stat => "stat",
            Kind::Timeseries => "timeseries",
            Kind::Gauge => "gauge",
            Kind::Bar => "bar",
            Kind::List => "list",
            Kind::Text => "text",
            Kind::Status => "status",
        }
    }

    fn has_rows(self) -> bool {
        matches!(self, Kind::Bar | Kind::List | Kind::Status)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Prometheus,
    HttpJson,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    /// The level applies from this value up (to the next threshold).
    pub value: f64,
    pub level: Level,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RowConfig {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SeriesConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `sparkline = true` (the panel's query over the last hour) or a table.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Sparkline {
    On(bool),
    Spec(SparklineSpec),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SparklineSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    /// Stable id; default derived from the section and panel titles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Default: `http_json` when `url` is set, else `prometheus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceKind>,
    /// PromQL (prometheus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// URL to GET (http_json).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// JSON pointer into the response (http_json), e.g. "/data/temp".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Row label: a Prometheus label name, or a key of each JSON array item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Value key of each JSON array item (default "value").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<RowConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<SeriesConfig>,
    /// Timeseries window and step (prometheus), e.g. "24h" / "1h".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparkline: Option<Sparkline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<Threshold>,
    /// Multiply every value by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Round every value to this many decimal places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    /// Status: a number at or above this is up (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_when: Option<f64>,
    /// Status: a string equal to this (case-insensitive) is up (default:
    /// ok, up, true, healthy, online, running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_text: Option<String>,
    /// Text: a fixed line instead of a query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

impl PanelConfig {
    pub fn source(&self) -> SourceKind {
        self.source.unwrap_or(if self.url.is_some() { SourceKind::HttpJson } else { SourceKind::Prometheus })
    }
}

/// Parse "500ms", "15s", "5m", "24h", "7d" or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().with_context(|| format!("bad duration '{s}' (use e.g. 15s, 5m, 24h)"))?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86_400.0,
        u => bail!("bad duration unit '{u}' in '{s}' (ms, s, m, h, d)"),
    };
    if !secs.is_finite() || secs <= 0.0 {
        bail!("duration '{s}' must be positive");
    }
    Ok(Duration::from_secs_f64(secs))
}

fn dur_or(v: Option<&str>, default: Duration) -> Result<Duration> {
    v.map(parse_duration).transpose().map(|d| d.unwrap_or(default))
}

/// Parse a TOML config.
pub fn parse(text: &str) -> Result<DashboardConfig> {
    Ok(toml::from_str(text)?)
}

/// Where the config lives: `HIVE_DASHBOARD_FILE`, else `<data dir>/dashboard.toml`.
pub fn config_path(data_dir: &Path) -> PathBuf {
    crate::env_opt("HIVE_DASHBOARD_FILE").map(PathBuf::from).unwrap_or_else(|| data_dir.join(FILE_NAME))
}

/// What the host serves from.
#[derive(Debug, Clone)]
pub enum Loaded {
    Config(DashboardConfig),
    /// No file: the generated default (Prometheus configured) or nothing.
    Default(DashboardConfig),
    Empty,
    Invalid(String),
}

/// Load the dashboard definition.
pub fn load(path: &Path, prometheus_configured: bool) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(cfg) => Loaded::Config(cfg),
            Err(e) => Loaded::Invalid(format!("{}: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if prometheus_configured {
                Loaded::Default(default_config(lab::temp_queries()))
            } else {
                Loaded::Empty
            }
        }
        Err(e) => Loaded::Invalid(format!("{}: {e}", path.display())),
    }
}

// ── Validation ─────────────────────────────────────────────────────────────

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Every panel's effective id (explicit, or `<section>-<panel>` slugs made unique).
pub fn panel_ids(cfg: &DashboardConfig) -> Vec<Vec<String>> {
    let mut seen: HashSet<String> = cfg
        .sections
        .iter()
        .flat_map(|s| s.panels.iter().filter_map(|p| p.id.clone()))
        .collect();
    cfg.sections
        .iter()
        .map(|s| {
            s.panels
                .iter()
                .map(|p| {
                    if let Some(id) = &p.id {
                        return id.clone();
                    }
                    let base = format!("{}-{}", slug(&s.title), slug(&p.title)).trim_matches('-').to_string();
                    let base = if base.is_empty() { "panel".to_string() } else { base };
                    let mut id = base.clone();
                    let mut n = 2;
                    while !seen.insert(id.clone()) {
                        id = format!("{base}-{n}");
                        n += 1;
                    }
                    id
                })
                .collect()
        })
        .collect()
}

fn check_pointer(p: &str) -> Result<()> {
    if !(p.is_empty() || p.starts_with('/')) {
        bail!("path '{p}' must be a JSON pointer starting with '/' (e.g. /data/value)");
    }
    Ok(())
}

fn need(v: &Option<String>, what: &str) -> Result<()> {
    match v {
        Some(s) if !s.trim().is_empty() => Ok(()),
        _ => bail!("needs {what}"),
    }
}

/// Check one panel; the error says what to fix.
pub fn validate_panel(p: &PanelConfig, defaults: &DashboardConfig) -> Result<()> {
    if p.title.trim().is_empty() {
        bail!("needs a title");
    }
    let timeout = dur_or(p.timeout.as_deref().or(defaults.timeout.as_deref()), DEFAULT_TIMEOUT)?;
    if timeout > MAX_TIMEOUT {
        bail!("timeout is at most {}s", MAX_TIMEOUT.as_secs());
    }
    if dur_or(p.refresh.as_deref().or(defaults.refresh.as_deref()), DEFAULT_REFRESH)? < MIN_REFRESH {
        bail!("refresh is at least 1s");
    }
    if let Some(r) = p.round {
        if r > 10 {
            bail!("round is at most 10 decimal places");
        }
    }
    if let Some(s) = p.scale {
        if !s.is_finite() {
            bail!("scale must be a finite number");
        }
    }
    if p.thresholds.windows(2).any(|w| w[1].value < w[0].value) {
        bail!("thresholds must be in ascending order of value");
    }
    if p.kind == Kind::Gauge {
        let (lo, hi) = (p.min.unwrap_or(0.0), p.max.unwrap_or(100.0));
        if lo >= hi {
            bail!("gauge needs min < max");
        }
    }
    let src = p.source();
    match src {
        SourceKind::HttpJson => {
            need(&p.url, "a url (http_json)")?;
            let url = p.url.as_deref().unwrap_or_default();
            let parsed = reqwest::Url::parse(url).with_context(|| format!("bad url '{url}'"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                bail!("url must be http or https");
            }
            if p.query.is_some() {
                bail!("query is for prometheus panels; http_json uses path");
            }
            for path in [&p.path].into_iter().chain(p.rows.iter().map(|r| &r.path)).chain(p.series.iter().map(|s| &s.path)).flatten() {
                check_pointer(path)?;
            }
        }
        SourceKind::Prometheus => {
            if p.url.is_some() || p.path.is_some() {
                bail!("url/path are for http_json panels; prometheus uses query");
            }
        }
    }
    let row_or_series_ok = |q: &Option<String>, path: &Option<String>| match src {
        SourceKind::Prometheus => need(q, "a query"),
        SourceKind::HttpJson => path.as_deref().map(check_pointer).unwrap_or_else(|| bail!("needs a path")),
    };
    match p.kind {
        Kind::Timeseries => {
            if p.series.is_empty() && src == SourceKind::Prometheus {
                need(&p.query, "a query or 1-3 series")?;
            } else if p.series.is_empty() {
                bail!("needs 1-3 series");
            }
            if p.series.len() > MAX_SERIES {
                bail!("at most {MAX_SERIES} series");
            }
            for s in &p.series {
                if s.name.trim().is_empty() {
                    bail!("every series needs a name");
                }
                row_or_series_ok(&s.query, &s.path).with_context(|| format!("series '{}'", s.name))?;
            }
            if src == SourceKind::Prometheus {
                range_step(p.range.as_deref(), p.step.as_deref(), "24h")?;
            }
        }
        k if k.has_rows() => {
            if p.rows.is_empty() {
                match src {
                    SourceKind::Prometheus => need(&p.query, "a query or rows")?,
                    SourceKind::HttpJson => {}
                }
            }
            for r in &p.rows {
                if r.label.trim().is_empty() {
                    bail!("every row needs a label");
                }
                row_or_series_ok(&r.query, &r.path).with_context(|| format!("row '{}'", r.label))?;
            }
        }
        Kind::Text if p.text.is_some() => {}
        _ => {
            if src == SourceKind::Prometheus {
                need(&p.query, "a query")?;
            }
            if !p.rows.is_empty() || !p.series.is_empty() {
                bail!("rows/series are for bar, list, status and timeseries panels");
            }
        }
    }
    if let Some(Sparkline::Spec(s)) = &p.sparkline {
        if src != SourceKind::Prometheus {
            bail!("sparklines need a prometheus source");
        }
        range_step(s.range.as_deref(), s.step.as_deref(), "1h")?;
    } else if p.sparkline == Some(Sparkline::On(true)) && src != SourceKind::Prometheus {
        bail!("sparklines need a prometheus source");
    }
    Ok(())
}

/// (range, step), with step defaulting to range/60 and at most 1000 points.
fn range_step(range: Option<&str>, step: Option<&str>, default_range: &str) -> Result<(Duration, Duration)> {
    let range = parse_duration(range.unwrap_or(default_range))?;
    let step = match step {
        Some(s) => parse_duration(s)?,
        None => Duration::from_secs((range.as_secs() / 60).max(1)),
    };
    if step.as_secs() == 0 {
        bail!("step is at least 1s");
    }
    if range.as_secs() / step.as_secs() > MAX_POINTS {
        bail!("range/step gives more than {MAX_POINTS} points");
    }
    Ok((range, step))
}

/// Every problem in the config, prefixed with where it is. Empty = valid.
pub fn validate(cfg: &DashboardConfig) -> Vec<String> {
    let mut out = Vec::new();
    for (what, v) in [("refresh", &cfg.refresh), ("timeout", &cfg.timeout)] {
        if let Some(Err(e)) = v.as_deref().map(parse_duration) {
            out.push(format!("{what}: {e}"));
        }
    }
    let mut ids = HashSet::new();
    for s in &cfg.sections {
        for p in &s.panels {
            if let Some(id) = &p.id {
                if !ids.insert(id.clone()) {
                    out.push(format!("[{}] {}: duplicate id '{id}'", s.title, p.title));
                }
            }
        }
    }
    for s in &cfg.sections {
        if s.title.trim().is_empty() {
            out.push("a section needs a title".into());
        }
        if s.show_when.as_deref().is_some_and(|q| q.trim().is_empty()) {
            out.push(format!("[{}] show_when is empty", s.title));
        }
        for p in &s.panels {
            if let Err(e) = validate_panel(p, cfg) {
                out.push(format!("[{}] {}: {e:#}", s.title, p.title));
            }
        }
    }
    out
}

// ── Evaluation ─────────────────────────────────────────────────────────────

/// Where panels get their data.
pub struct Sources<'a> {
    pub http: &'a reqwest::Client,
    pub prometheus_url: &'a str,
}

fn now_iso() -> String {
    super::api::iso(hive_core::Timestamp::now())
}

fn unix_iso(t: f64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos((t * 1e9) as i128)
        .ok()
        .and_then(|d| d.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

struct Transform {
    scale: f64,
    round: Option<u32>,
}

impl Transform {
    fn of(p: &PanelConfig) -> Self {
        Self { scale: p.scale.unwrap_or(1.0), round: p.round }
    }

    fn apply(&self, v: f64) -> f64 {
        let v = v * self.scale;
        match self.round {
            Some(places) => {
                let f = 10f64.powi(places as i32);
                (v * f).round() / f
            }
            None => v,
        }
    }
}

fn level_for(v: f64, thresholds: &[Threshold]) -> Option<Level> {
    thresholds.iter().rev().find(|t| v >= t.value).map(|t| t.level)
}

async fn prom_get(src: &Sources<'_>, path: &str, params: &[(&str, String)], timeout: Duration) -> Result<Value> {
    let url = format!("{}/api/v1/{path}", src.prometheus_url.trim_end_matches('/'));
    let resp = src
        .http
        .get(url)
        .query(params)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Prometheus unreachable: {}", without_url(&e)))?;
    let status = resp.status();
    let body: Value = resp.json().await.context("Prometheus sent a non-JSON reply")?;
    if body.get("status").and_then(Value::as_str) != Some("success") {
        let msg = body.get("error").and_then(Value::as_str).unwrap_or("query failed");
        bail!("Prometheus: {msg} (HTTP {})", status.as_u16());
    }
    Ok(body)
}

/// A reqwest error without the URL (which can carry a query or credentials).
fn without_url(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        let s = e.to_string();
        s.split(" for url").next().unwrap_or(&s).to_string()
    }
}

type Sample = (Map<String, Value>, f64);

async fn instant(src: &Sources<'_>, promql: &str, timeout: Duration) -> Result<Vec<Sample>> {
    let body = prom_get(src, "query", &[("query", promql.to_string())], timeout).await?;
    // A scalar result: [t, "v"].
    if body.pointer("/data/resultType").and_then(Value::as_str) == Some("scalar") {
        let v = body.pointer("/data/result/1").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
        return Ok(v.filter(|v| v.is_finite()).map(|v| vec![(Map::new(), v)]).unwrap_or_default());
    }
    lab::parse_vector(&body).context("unexpected Prometheus reply")
}

async fn instant_one(src: &Sources<'_>, promql: &str, timeout: Duration) -> Result<f64> {
    instant(src, promql, timeout).await?.first().map(|(_, v)| *v).context("no data")
}

async fn range_query(src: &Sources<'_>, promql: &str, range: Duration, step: Duration, timeout: Duration) -> Result<Vec<(f64, f64)>> {
    let step_s = step.as_secs().max(1) as i64;
    let end = time::OffsetDateTime::now_utc().unix_timestamp() / step_s * step_s;
    let start = end - range.as_secs() as i64;
    let params = [
        ("query", promql.to_string()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step_s.to_string()),
    ];
    let body = prom_get(src, "query_range", &params, timeout).await?;
    let pts = lab::parse_matrix_first(&body).context("unexpected Prometheus reply")?;
    if pts.is_empty() {
        bail!("no data");
    }
    Ok(pts.into_iter().map(|(t, v)| (t as f64, v)).collect())
}

/// GET a JSON document (at most 1 MiB).
async fn fetch_json(src: &Sources<'_>, url: &str, timeout: Duration) -> Result<Value> {
    let mut resp = src
        .http
        .get(url)
        .header("accept", "application/json")
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("GET failed: {}", without_url(&e)))?;
    if !resp.status().is_success() {
        bail!("GET returned HTTP {}", resp.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| anyhow::anyhow!("reading the reply: {}", without_url(&e)))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_JSON_BYTES {
            bail!("reply is larger than 1 MiB");
        }
    }
    serde_json::from_slice(&body).context("reply isn't JSON")
}

fn pointer<'v>(doc: &'v Value, path: Option<&str>) -> Result<&'v Value> {
    match path.unwrap_or("") {
        "" | "/" => Ok(doc),
        p => doc.pointer(p).with_context(|| format!("nothing at {p}")),
    }
}

fn json_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
    .filter(|v| v.is_finite())
}

fn json_time(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().map(|t| if t > 1e12 { t / 1000.0 } else { t }),
        Value::String(s) => time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|d| d.unix_timestamp_nanos() as f64 / 1e9)
            .or_else(|| s.parse::<f64>().ok()),
        _ => None,
    }
}

/// A JSON series: `[[t, v], …]` or `[{t|time|timestamp, v|value}, …]`.
fn json_points(v: &Value, value_key: Option<&str>) -> Result<Vec<(f64, f64)>> {
    let arr = v.as_array().context("expected an array of points")?;
    let pts: Vec<(f64, f64)> = arr
        .iter()
        .filter_map(|p| match p {
            Value::Array(a) => Some((json_time(a.first()?)?, json_number(a.get(1)?)?)),
            Value::Object(o) => {
                let t = ["t", "time", "timestamp"].iter().find_map(|k| o.get(*k)).and_then(json_time)?;
                let val = match value_key {
                    Some(k) => o.get(k),
                    None => o.get("v").or_else(|| o.get("value")),
                };
                Some((t, json_number(val?)?))
            }
            _ => None,
        })
        .collect();
    if pts.is_empty() {
        bail!("no points");
    }
    Ok(pts)
}

fn sample_label(labels: &Map<String, Value>, key: Option<&str>, fallback: &str) -> String {
    if let Some(k) = key {
        return labels.get(k).and_then(Value::as_str).unwrap_or(fallback).to_string();
    }
    let parts: Vec<&str> = labels
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .filter_map(|(_, v)| v.as_str())
        .collect();
    if parts.is_empty() {
        labels.get("__name__").and_then(Value::as_str).unwrap_or(fallback).to_string()
    } else {
        parts.join(", ")
    }
}

/// A raw row before transforms: label and a number or a string.
enum Raw {
    Num(f64),
    Str(String),
}

fn is_up(p: &PanelConfig, raw: &Raw) -> bool {
    match raw {
        Raw::Num(v) => *v >= p.up_when.unwrap_or(1.0),
        Raw::Str(s) => {
            let s = s.trim().to_ascii_lowercase();
            match &p.up_text {
                Some(t) => s == t.trim().to_ascii_lowercase(),
                None => matches!(s.as_str(), "ok" | "up" | "true" | "healthy" | "online" | "running" | "pass"),
            }
        }
    }
}

fn raw_of(v: &Value) -> Option<Raw> {
    match v {
        Value::String(s) => Some(s.trim().parse::<f64>().ok().filter(|v| v.is_finite()).map(Raw::Num).unwrap_or(Raw::Str(s.clone()))),
        other => json_number(other).map(Raw::Num),
    }
}

/// Collect the rows of a bar/list/status panel.
async fn rows(p: &PanelConfig, src: &Sources<'_>, timeout: Duration) -> Result<Vec<(String, Raw)>> {
    match p.source() {
        SourceKind::Prometheus if p.rows.is_empty() => {
            let mut out: Vec<(String, Raw)> = instant(src, p.query.as_deref().unwrap_or_default(), timeout)
                .await?
                .into_iter()
                .map(|(labels, v)| (labels, Raw::Num(v)))
                .map(|(labels, v)| (sample_label(&labels, p.label.as_deref(), &p.title), v))
                .collect();
            if out.is_empty() {
                bail!("no data");
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(out)
        }
        SourceKind::Prometheus => {
            let futs = p.rows.iter().map(|r| instant_one(src, r.query.as_deref().unwrap_or_default(), timeout));
            let results = futures_util::future::join_all(futs).await;
            let mut out = Vec::new();
            let mut first_err = None;
            for (r, res) in p.rows.iter().zip(results) {
                match res {
                    Ok(v) => out.push((r.label.clone(), Raw::Num(v))),
                    Err(e) => {
                        first_err.get_or_insert(e);
                    }
                }
            }
            match (out.is_empty(), first_err) {
                (true, Some(e)) => Err(e),
                _ => Ok(out),
            }
        }
        SourceKind::HttpJson => {
            let doc = fetch_json(src, p.url.as_deref().unwrap_or_default(), timeout).await?;
            if !p.rows.is_empty() {
                let out: Vec<(String, Raw)> = p
                    .rows
                    .iter()
                    .filter_map(|r| Some((r.label.clone(), raw_of(pointer(&doc, r.path.as_deref()).ok()?)?)))
                    .collect();
                if out.is_empty() {
                    bail!("no row paths matched");
                }
                return Ok(out);
            }
            let at = pointer(&doc, p.path.as_deref())?;
            let vk = p.value_key.as_deref().unwrap_or("value");
            let out: Vec<(String, Raw)> = match at {
                Value::Array(items) => items
                    .iter()
                    .filter_map(|it| {
                        let o = it.as_object()?;
                        let label = match p.label.as_deref() {
                            Some(k) => o.get(k),
                            None => o.get("name").or_else(|| o.get("label")),
                        };
                        let label = match label? {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        Some((label, raw_of(o.get(vk)?)?))
                    })
                    .collect(),
                Value::Object(o) => o.iter().filter_map(|(k, v)| Some((k.clone(), raw_of(v)?))).collect(),
                other => vec![(p.title.clone(), raw_of(other).context("not a number or string")?)],
            };
            if out.is_empty() {
                bail!("no rows at {}", p.path.as_deref().unwrap_or("/"));
            }
            Ok(out)
        }
    }
}

/// A single value: (number, text).
async fn single(p: &PanelConfig, src: &Sources<'_>, timeout: Duration) -> Result<Raw> {
    match p.source() {
        SourceKind::Prometheus => Ok(Raw::Num(instant_one(src, p.query.as_deref().unwrap_or_default(), timeout).await?)),
        SourceKind::HttpJson => {
            let doc = fetch_json(src, p.url.as_deref().unwrap_or_default(), timeout).await?;
            let at = pointer(&doc, p.path.as_deref())?;
            raw_of(at).with_context(|| format!("{} isn't a number or string", p.path.as_deref().unwrap_or("/")))
        }
    }
}

fn points_json(pts: &[(f64, f64)], tf: &Transform) -> Value {
    Value::Array(pts.iter().map(|(t, v)| json!({ "t": unix_iso(*t), "v": tf.apply(*v) })).collect())
}

/// Evaluate a panel's data into `out`.
async fn fill(p: &PanelConfig, src: &Sources<'_>, timeout: Duration, out: &mut Map<String, Value>) -> Result<()> {
    let tf = Transform::of(p);
    match p.kind {
        Kind::Stat | Kind::Gauge => {
            let raw = single(p, src, timeout).await?;
            let Raw::Num(v) = raw else { bail!("expected a number") };
            let v = tf.apply(v);
            out.insert("value".into(), json!(v));
            if let Some(l) = level_for(v, &p.thresholds) {
                out.insert("level".into(), json!(l));
            }
            if p.kind == Kind::Stat {
                let spark = match &p.sparkline {
                    Some(Sparkline::On(true)) => Some(SparklineSpec::default()),
                    Some(Sparkline::Spec(s)) => Some(s.clone()),
                    _ => None,
                };
                if let Some(s) = spark {
                    let (range, step) = range_step(s.range.as_deref(), s.step.as_deref(), "1h")?;
                    let q = s.query.as_deref().or(p.query.as_deref()).unwrap_or_default();
                    // A sparkline is decoration: its failure doesn't fail the stat.
                    if let Ok(pts) = range_query(src, q, range, step, timeout).await {
                        out.insert("sparkline".into(), points_json(&pts, &tf));
                    }
                }
            }
        }
        Kind::Text => {
            if let Some(t) = &p.text {
                out.insert("text".into(), json!(t));
                return Ok(());
            }
            match single(p, src, timeout).await? {
                Raw::Num(v) => {
                    let v = tf.apply(v);
                    out.insert("value".into(), json!(v));
                    if let Some(l) = level_for(v, &p.thresholds) {
                        out.insert("level".into(), json!(l));
                    }
                }
                Raw::Str(s) => {
                    out.insert("text".into(), json!(s));
                }
            }
        }
        Kind::Timeseries => {
            let series: Vec<SeriesConfig> = if p.series.is_empty() {
                vec![SeriesConfig { name: p.title.clone(), query: p.query.clone(), path: None }]
            } else {
                p.series.clone()
            };
            let results: Vec<Result<Vec<(f64, f64)>>> = match p.source() {
                SourceKind::Prometheus => {
                    let (range, step) = range_step(p.range.as_deref(), p.step.as_deref(), "24h")?;
                    futures_util::future::join_all(
                        series.iter().map(|s| range_query(src, s.query.as_deref().unwrap_or_default(), range, step, timeout)),
                    )
                    .await
                }
                SourceKind::HttpJson => {
                    let doc = fetch_json(src, p.url.as_deref().unwrap_or_default(), timeout).await?;
                    series
                        .iter()
                        .map(|s| json_points(pointer(&doc, s.path.as_deref())?, p.value_key.as_deref()))
                        .collect()
                }
            };
            let mut arr = Vec::new();
            let mut first_err = None;
            for (s, r) in series.iter().zip(results) {
                match r {
                    Ok(pts) => arr.push(json!({ "name": s.name, "points": points_json(&pts, &tf) })),
                    Err(e) => {
                        first_err.get_or_insert(e.context(format!("series '{}'", s.name)));
                    }
                }
            }
            if arr.is_empty() {
                return Err(first_err.unwrap_or_else(|| anyhow::anyhow!("no data")));
            }
            out.insert("series".into(), Value::Array(arr));
        }
        Kind::Bar | Kind::List | Kind::Status => {
            let rows = rows(p, src, timeout).await?;
            let arr: Vec<Value> = rows
                .into_iter()
                .map(|(label, raw)| {
                    let mut r = json!({ "label": label });
                    if p.kind == Kind::Status {
                        r["up"] = json!(is_up(p, &raw));
                    }
                    match raw {
                        Raw::Num(v) => {
                            let v = tf.apply(v);
                            r["value"] = json!(v);
                            if let Some(l) = level_for(v, &p.thresholds) {
                                r["level"] = json!(l);
                            }
                        }
                        Raw::Str(s) => r["text"] = json!(s),
                    }
                    r
                })
                .collect();
            out.insert("rows".into(), Value::Array(arr));
        }
    }
    Ok(())
}

/// For a status panel whose source failed: every configured row reads down.
fn down_rows(p: &PanelConfig) -> Option<Value> {
    if p.kind != Kind::Status {
        return None;
    }
    let labels: Vec<String> = if p.rows.is_empty() {
        // A single-target check (one URL, or a query with no label split).
        if p.source() == SourceKind::HttpJson || p.label.is_none() {
            vec![p.title.clone()]
        } else {
            return None;
        }
    } else {
        p.rows.iter().map(|r| r.label.clone()).collect()
    };
    Some(Value::Array(labels.into_iter().map(|l| json!({ "label": l, "up": false })).collect()))
}

/// Evaluate one panel: never fails, a problem becomes `error`.
pub async fn eval_panel(id: &str, p: &PanelConfig, defaults: &DashboardConfig, src: &Sources<'_>) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("title".into(), json!(p.title));
    out.insert("kind".into(), json!(p.kind.as_str()));
    if let Some(u) = &p.unit {
        out.insert("unit".into(), json!(u));
    }
    if p.kind == Kind::Gauge || p.min.is_some() {
        out.insert("min".into(), json!(p.min.unwrap_or(0.0)));
    }
    if p.kind == Kind::Gauge || p.max.is_some() {
        out.insert("max".into(), json!(p.max.unwrap_or(100.0)));
    }
    if !p.thresholds.is_empty() {
        out.insert("thresholds".into(), json!(p.thresholds));
    }
    let refresh = dur_or(p.refresh.as_deref().or(defaults.refresh.as_deref()), DEFAULT_REFRESH).unwrap_or(DEFAULT_REFRESH);
    out.insert("refreshSeconds".into(), json!(refresh.as_secs_f64()));
    let result = match validate_panel(p, defaults) {
        Err(e) => Err(e.context("invalid panel")),
        Ok(()) => {
            let timeout = dur_or(p.timeout.as_deref().or(defaults.timeout.as_deref()), DEFAULT_TIMEOUT).unwrap_or(DEFAULT_TIMEOUT);
            match tokio::time::timeout(timeout + Duration::from_millis(250), fill(p, src, timeout, &mut out)).await {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!("timed out after {}s", timeout.as_secs_f64())),
            }
        }
    };
    if let Err(e) = result {
        out.insert("error".into(), json!(format!("{e:#}")));
        if let Some(rows) = down_rows(p) {
            out.insert("rows".into(), rows);
        }
    }
    out.insert("updatedAt".into(), json!(now_iso()));
    Value::Object(out)
}

/// Cached panel results: key → (when, value, lifetime).
#[derive(Default)]
pub struct Cache(Mutex<HashMap<String, (Instant, Value, Duration)>>);

impl Cache {
    fn get(&self, key: &str) -> Option<Value> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(key).filter(|(at, _, ttl)| at.elapsed() < *ttl).map(|(_, v, _)| v.clone())
    }

    fn put(&self, key: String, v: Value, ttl: Duration) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (at, _, ttl)| at.elapsed() < *ttl * 4);
        m.insert(key, (Instant::now(), v, ttl));
    }
}

/// Evaluate a whole config (panels concurrently; fresh cache entries reused).
pub async fn evaluate(cfg: &DashboardConfig, src: &Sources<'_>, cache: Option<&Cache>) -> Value {
    let ids = panel_ids(cfg);
    let futs = cfg.sections.iter().zip(&ids).map(|(s, ids)| async move {
        if let Some(q) = s.show_when.as_deref() {
            let timeout = dur_or(cfg.timeout.as_deref(), DEFAULT_TIMEOUT).unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
            let key = format!("{}|show_when|{q}", src.prometheus_url);
            let show = match cache.and_then(|c| c.get(&key)) {
                Some(v) => v.as_bool() == Some(true),
                None => {
                    let show = matches!(instant(src, q, timeout).await, Ok(rows) if !rows.is_empty());
                    if let Some(c) = cache {
                        let refresh = dur_or(cfg.refresh.as_deref(), DEFAULT_REFRESH).unwrap_or(DEFAULT_REFRESH);
                        c.put(key, json!(show), refresh);
                    }
                    show
                }
            };
            if !show {
                return None;
            }
        }
        let panels = futures_util::future::join_all(s.panels.iter().zip(ids).map(|(p, id)| async move {
            let key = format!("{}|{id}|{}", src.prometheus_url, serde_json::to_string(p).unwrap_or_default());
            if let Some(v) = cache.and_then(|c| c.get(&key)) {
                return v;
            }
            let v = eval_panel(id, p, cfg, src).await;
            if let Some(c) = cache {
                let refresh = v["refreshSeconds"].as_f64().map(Duration::from_secs_f64).unwrap_or(DEFAULT_REFRESH);
                let ttl = if v.get("error").is_some() { refresh.min(ERROR_RETRY) } else { refresh };
                c.put(key, v.clone(), ttl);
            }
            v
        }))
        .await;
        Some((s, panels))
    });
    let sections: Vec<Value> = futures_util::future::join_all(futs)
        .await
        .into_iter()
        .flatten()
        .filter(|(s, panels)| !(s.optional && panels.iter().all(|p| p.get("error").is_some())))
        .map(|(s, panels)| json!({ "title": s.title, "panels": panels }))
        .collect();
    let default_refresh = dur_or(cfg.refresh.as_deref(), DEFAULT_REFRESH).unwrap_or(DEFAULT_REFRESH);
    let refresh = cfg
        .sections
        .iter()
        .flat_map(|s| &s.panels)
        .map(|p| p.refresh.as_deref().and_then(|r| parse_duration(r).ok()).unwrap_or(default_refresh))
        .min()
        .unwrap_or(default_refresh);
    json!({
        "title": cfg.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| DEFAULT_TITLE.into()),
        "refreshSeconds": refresh.as_secs_f64().max(MIN_REFRESH.as_secs_f64()),
        "sections": sections,
        "updatedAt": now_iso(),
    })
}

/// The `/v1/dashboard` body for what [`load`] found.
pub async fn render(loaded: &Loaded, src: &Sources<'_>, cache: Option<&Cache>) -> Value {
    let (mut v, source) = match loaded {
        Loaded::Config(c) => (evaluate(c, src, cache).await, "config"),
        Loaded::Default(c) => (evaluate(c, src, cache).await, "default"),
        Loaded::Empty => (evaluate(&DashboardConfig::default(), src, None).await, "none"),
        Loaded::Invalid(e) => {
            let mut v = evaluate(&DashboardConfig::default(), src, None).await;
            v["error"] = json!(e);
            (v, "config")
        }
    };
    v["source"] = json!(source);
    v
}

// ── Default dashboard (the former /v1/lab/status) ──────────────────────────

fn prom(title: &str, kind: Kind, unit: &str, query: &str) -> PanelConfig {
    PanelConfig {
        title: title.into(),
        kind,
        unit: (!unit.is_empty()).then(|| unit.to_string()),
        query: Some(query.into()),
        ..PanelConfig::default()
    }
}

fn rounded(mut p: PanelConfig, places: u32) -> PanelConfig {
    p.round = Some(places);
    p
}

/// What `/v1/lab/status` showed, as a dashboard: power and cost, gateway
/// workers, temperatures (`HIVE_LAB_TEMPS`), and the solar offset when an
/// Enphase exporter reports in. Every section is optional, so a host without
/// those metrics shows only what it has.
pub fn default_config(temps: Vec<(String, String)>) -> DashboardConfig {
    let kwh24 = |q: &str| format!("sum_over_time({q}[24h:1m]) / 60 / 1000");
    let mut power = rounded(prom("Power", Kind::Stat, "W", "lab:power_watts:sum"), 1);
    power.sparkline = Some(Sparkline::Spec(SparklineSpec { query: None, range: Some("6h".into()), step: Some("5m".into()) }));
    let temp_thresholds = vec![
        Threshold { value: 0.0, level: Level::Ok },
        Threshold { value: 55.0, level: Level::Warn },
        Threshold { value: 85.0, level: Level::Critical },
    ];
    let mut offset = prom(
        "Lab solar offset, 24 h",
        Kind::Gauge,
        "%",
        &format!("clamp_max(({}) / ({}), 1)", kwh24("lab:solar_watts"), kwh24("lab:power_watts:sum")),
    );
    offset.scale = Some(100.0);
    offset.round = Some(0);
    offset.min = Some(0.0);
    offset.max = Some(100.0);
    let mut history = prom("Solar vs lab, 24 h", Kind::Timeseries, "W", "");
    history.query = None;
    history.round = Some(0);
    history.range = Some("24h".into());
    history.step = Some("1h".into());
    history.series = vec![
        SeriesConfig { name: "Solar".into(), query: Some("avg_over_time(clamp_min(envoy_production_watts, 0)[1h:1m])".into()), path: None },
        SeriesConfig { name: "Lab".into(), query: Some("avg_over_time(lab:power_watts:sum[1h:1m])".into()), path: None },
    ];
    let mut solar_value = rounded(
        prom("Lab solar value, 24 h", Kind::Stat, "$", &format!("({}) * scalar(lab:electric_rate_usd_per_kwh)", kwh24("lab:solar_watts"))),
        2,
    );
    solar_value.refresh = Some("60s".into());
    let slow = |mut p: PanelConfig| {
        p.refresh = Some("60s".into());
        p
    };
    DashboardConfig {
        title: Some("Lab".into()),
        refresh: Some("10s".into()),
        timeout: Some("2s".into()),
        sections: vec![
            SectionConfig {
                title: "Power".into(),
                optional: true,
                show_when: None,
                panels: vec![
                    power,
                    rounded(prom("Cost / hour", Kind::Stat, "$/h", "lab:cost_usd_per_hour"), 3),
                    rounded(prom("Cost / month", Kind::Stat, "$/mo", "lab:cost_usd_per_hour * 720"), 2),
                    rounded(prom("Rate", Kind::Stat, "$/kWh", "lab:electric_rate_usd_per_kwh"), 3),
                ],
            },
            SectionConfig {
                title: "Workers".into(),
                optional: true,
                show_when: None,
                panels: vec![PanelConfig { label: Some("worker".into()), ..prom("Workers", Kind::Status, "", "rdnagw_worker_up") }],
            },
            SectionConfig {
                title: "Temperatures".into(),
                optional: true,
                show_when: None,
                panels: vec![PanelConfig {
                    rows: temps.into_iter().map(|(label, q)| RowConfig { label, query: Some(q), path: None }).collect(),
                    max: Some(100.0),
                    round: Some(0),
                    thresholds: temp_thresholds,
                    query: None,
                    ..prom("Temperatures", Kind::Bar, "°C", "")
                }],
            },
            SectionConfig {
                title: "Solar".into(),
                optional: true,
                show_when: Some("envoy_up".into()),
                panels: vec![
                    rounded(prom("Solar now", Kind::Stat, "W", "clamp_min(envoy_production_watts, 0)"), 0),
                    rounded(prom("House now", Kind::Stat, "W", "envoy_consumption_watts"), 0),
                    rounded(prom("Grid now", Kind::Stat, "W", "envoy_grid_watts"), 0),
                    rounded(prom("Lab on solar now", Kind::Stat, "W", "lab:solar_watts"), 0),
                    slow(offset),
                    slow(history),
                    slow(rounded(prom("Produced, 24 h", Kind::Stat, "kWh", &kwh24("clamp_min(envoy_production_watts, 0)")), 1)),
                    slow(rounded(prom("Lab used, 24 h", Kind::Stat, "kWh", &kwh24("lab:power_watts:sum")), 2)),
                    slow(rounded(prom("Lab from solar, 24 h", Kind::Stat, "kWh", &kwh24("lab:solar_watts")), 2)),
                    solar_value,
                    slow(rounded(prom("Grid import, 24 h", Kind::Stat, "kWh", &kwh24("clamp_min(envoy_grid_watts, 0)")), 1)),
                    slow(rounded(prom("Grid export, 24 h", Kind::Stat, "kWh", &kwh24("clamp_min(-envoy_grid_watts, 0)")), 1)),
                ],
            },
        ],
    }
}

/// The default config as TOML (a starting point for `dashboard.toml`).
pub fn default_toml() -> Result<String> {
    Ok(toml::to_string(&default_config(lab::temp_queries()))?)
}

// ── `hive dashboard` ───────────────────────────────────────────────────────

/// `hive dashboard check [--file PATH]`: validate, evaluate once, print.
/// Fails when the config is invalid or any panel has an error.
pub fn cmd_check(data_dir: &Path, file: Option<String>) -> Result<()> {
    let path = file.map(PathBuf::from).unwrap_or_else(|| config_path(data_dir));
    let prometheus_url = crate::env_opt("HIVE_PROMETHEUS_URL");
    let loaded = load(&path, prometheus_url.is_some());
    let cfg = match &loaded {
        Loaded::Config(c) => {
            println!("{}", path.display());
            c.clone()
        }
        Loaded::Default(c) => {
            println!("no {} — the generated default (HIVE_PROMETHEUS_URL is set):", path.display());
            c.clone()
        }
        Loaded::Empty => {
            println!(
                "no {} and HIVE_PROMETHEUS_URL isn't set: the dashboard is empty (the app hides its tab).",
                path.display()
            );
            return Ok(());
        }
        Loaded::Invalid(e) => bail!("{e}"),
    };
    let problems = validate(&cfg);
    for p in &problems {
        println!("  invalid: {p}");
    }
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).build()?;
    let base = prometheus_url.unwrap_or_else(|| "http://127.0.0.1:9090".into());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let out = rt.block_on(render(&loaded, &Sources { http: &http, prometheus_url: &base }, None));
    println!("title: {}", out["title"].as_str().unwrap_or_default());
    let mut errors = 0;
    for s in out["sections"].as_array().into_iter().flatten() {
        println!("\n[{}]", s["title"].as_str().unwrap_or_default());
        for p in s["panels"].as_array().into_iter().flatten() {
            let head = format!("  {} ({}, {})", p["title"].as_str().unwrap_or_default(), p["kind"].as_str().unwrap_or_default(), p["id"].as_str().unwrap_or_default());
            if let Some(e) = p.get("error").and_then(Value::as_str) {
                errors += 1;
                println!("{head}: ERROR {e}");
            } else {
                println!("{head}: {}", summarize(p));
            }
        }
    }
    let shown: usize = out["sections"].as_array().map(|a| a.len()).unwrap_or(0);
    let hidden = cfg.sections.len().saturating_sub(shown);
    if hidden > 0 {
        println!("\n({hidden} optional section(s) hidden: no data)");
    }
    if !problems.is_empty() || errors > 0 {
        bail!("{} invalid panel(s), {errors} panel error(s)", problems.len());
    }
    println!("\nok");
    Ok(())
}

fn summarize(p: &Value) -> String {
    let unit = p["unit"].as_str().map(|u| format!(" {u}")).unwrap_or_default();
    if let Some(t) = p.get("text").and_then(Value::as_str) {
        return t.to_string();
    }
    if let Some(v) = p.get("value") {
        let spark = p.get("sparkline").and_then(Value::as_array).map(|a| format!(" (sparkline: {} points)", a.len())).unwrap_or_default();
        return format!("{v}{unit}{spark}");
    }
    if let Some(series) = p.get("series").and_then(Value::as_array) {
        return series
            .iter()
            .map(|s| format!("{}: {} points", s["name"].as_str().unwrap_or_default(), s["points"].as_array().map(|a| a.len()).unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(", ");
    }
    if let Some(rows) = p.get("rows").and_then(Value::as_array) {
        return rows
            .iter()
            .map(|r| {
                let v = r.get("up").map(|u| if u.as_bool() == Some(true) { "up".to_string() } else { "down".to_string() })
                    .or_else(|| r.get("value").map(|v| format!("{v}{unit}")))
                    .or_else(|| r.get("text").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                format!("{}={v}", r["label"].as_str().unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::routing::get;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    /// A fake Prometheus: instant queries by exact PromQL, range queries as a
    /// three-point ramp, "slow" hangs, "boom" is a query error. Counts calls.
    async fn fake_prometheus() -> (String, Arc<AtomicUsize>) {
        async fn instant_q(State(n): State<Arc<AtomicUsize>>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            n.fetch_add(1, Ordering::SeqCst);
            let query = q.get("query").cloned().unwrap_or_default();
            let vector = |rows: Value| json!({"status": "success", "data": {"resultType": "vector", "result": rows}});
            Json(match query.as_str() {
                "power" => vector(json!([{"metric": {}, "value": [1, "184.66"]}])),
                "share" => vector(json!([{"metric": {}, "value": [1, "0.625"]}])),
                "workers" => vector(json!([
                    {"metric": {"__name__": "workers", "worker": "b-gpu"}, "value": [1, "0"]},
                    {"metric": {"__name__": "workers", "worker": "a-cpu"}, "value": [1, "1"]}
                ])),
                "temp_a" => vector(json!([{"metric": {}, "value": [1, "49.25"]}])),
                "temp_hot" => vector(json!([{"metric": {}, "value": [1, "91"]}])),
                "scalar(1)" => json!({"status": "success", "data": {"resultType": "scalar", "result": [1, "1"]}}),
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    vector(json!([]))
                }
                "boom" => json!({"status": "error", "errorType": "bad_data", "error": "parse error"}),
                _ => vector(json!([])),
            })
        }
        async fn range_q(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
            let query = q.get("query").cloned().unwrap_or_default();
            if query == "empty" {
                return Json(json!({"status": "success", "data": {"resultType": "matrix", "result": []}}));
            }
            let start: i64 = q.get("start").and_then(|s| s.parse().ok()).unwrap_or(0);
            let step: i64 = q.get("step").and_then(|s| s.parse().ok()).unwrap_or(60);
            let k = if query.contains("lab") { 10.0 } else { 1.0 };
            let values: Vec<Value> = (0..3).map(|i| json!([start + i * step, format!("{}", k * (i as f64 + 1.5))])).collect();
            Json(json!({"status": "success", "data": {"resultType": "matrix", "result": [{"metric": {}, "values": values}]}}))
        }
        let n = Arc::new(AtomicUsize::new(0));
        let r = Router::new()
            .route("/api/v1/query", get(instant_q))
            .route("/api/v1/query_range", get(range_q))
            .with_state(n.clone());
        (spawn(r).await, n)
    }

    /// A fake JSON service on the host's network.
    async fn fake_json() -> String {
        let r = Router::new()
            .route(
                "/doc",
                get(|| async {
                    Json(json!({
                        "power": {"milliwatts": 12345, "label": "fine"},
                        "health": "OK",
                        "disks": [{"name": "sda", "used": 0.5}, {"name": "sdb", "used": "0.25"}, {"bogus": 1}],
                        "fans": {"cpu": 1200, "case": 800},
                        "history": [[1700000000, 1.0], [1700003600, "2.5"]],
                        "history_ms": [{"time": 1700000000000u64, "value": 3}, {"timestamp": "2023-11-14T23:13:20Z", "v": 4}]
                    }))
                }),
            )
            .route("/fail", get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "no") }))
            .route("/text", get(|| async { "not json" }));
        spawn(r).await
    }

    fn panel(toml_panel: &str) -> PanelConfig {
        let cfg = parse(&format!("[[section]]\ntitle = \"S\"\n[[section.panel]]\n{toml_panel}")).unwrap();
        cfg.sections[0].panels[0].clone()
    }

    async fn eval(p: &PanelConfig, prom: &str) -> Value {
        let http = reqwest::Client::new();
        eval_panel("p", p, &DashboardConfig::default(), &Sources { http: &http, prometheus_url: prom }).await
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("15s").unwrap(), Duration::from_secs(15));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
        assert_eq!(parse_duration("1.5").unwrap(), Duration::from_millis(1500));
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("5w").is_err());
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn config_parses_every_kind_and_derives_unique_ids() {
        let cfg = parse(
            r#"
            title = "Home"
            refresh = "20s"
            [[section]]
            title = "Box One"
            optional = true
            [[section.panel]]
            title = "CPU %"
            query = "cpu"
            sparkline = true
            [[section.panel]]
            title = "CPU %"
            kind = "gauge"
            query = "cpu"
            [[section.panel]]
            id = "net"
            title = "Net"
            kind = "timeseries"
            series = [{ name = "rx", query = "rx" }, { name = "tx", query = "tx" }]
            range = "6h"
            step = "5m"
            [[section.panel]]
            title = "Disks"
            kind = "bar"
            url = "http://127.0.0.1:1/x"
            path = "/disks"
            [[section.panel]]
            title = "Fans"
            kind = "list"
            rows = [{ label = "CPU", query = "fan" }]
            [[section.panel]]
            title = "Note"
            kind = "text"
            text = "hello"
            [[section.panel]]
            title = "Up"
            kind = "status"
            query = "up"
            label = "job"
            "#,
        )
        .unwrap();
        assert_eq!(validate(&cfg), Vec::<String>::new());
        assert!(cfg.sections[0].optional);
        assert_eq!(cfg.sections[0].panels[3].source(), SourceKind::HttpJson);
        assert_eq!(cfg.sections[0].panels[0].sparkline, Some(Sparkline::On(true)));
        let ids = panel_ids(&cfg);
        assert_eq!(ids[0][0], "box-one-cpu");
        assert_eq!(ids[0][1], "box-one-cpu-2");
        assert_eq!(ids[0][2], "net");
        assert_eq!(ids[0][3], "box-one-disks");
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(parse("[[section]]\ntitle = \"S\"\n[[section.panel]]\ntitle = \"x\"\nquery = \"q\"\ncommand = \"rm -rf /\"").is_err());
        assert!(parse("[[section]]\ntitle = \"S\"\n[[section.panel]]\ntitle = \"x\"\nkind = \"shell\"").is_err());
        assert!(parse("[[section]]\ntitle = \"S\"\n[[section.panel]]\ntitle = \"x\"\nsource = \"exec\"").is_err());
    }

    #[test]
    fn validation_says_what_to_fix() {
        let bad = |p: &str| {
            let p = panel(p);
            format!("{:#}", validate_panel(&p, &DashboardConfig::default()).unwrap_err())
        };
        assert!(bad("title = \"x\"").contains("needs a query"));
        assert!(bad("title = \"\"\nquery = \"q\"").contains("title"));
        assert!(bad("title = \"x\"\nquery = \"q\"\nrefresh = \"soon\"").contains("bad duration"));
        assert!(bad("title = \"x\"\nquery = \"q\"\ntimeout = \"1m\"").contains("at most 30s"));
        assert!(bad("title = \"x\"\nquery = \"q\"\nrefresh = \"100ms\"").contains("at least 1s"));
        assert!(bad("title = \"x\"\nkind = \"timeseries\"\nseries = [{name=\"a\",query=\"a\"},{name=\"b\",query=\"b\"},{name=\"c\",query=\"c\"},{name=\"d\",query=\"d\"}]").contains("at most 3"));
        assert!(bad("title = \"x\"\nkind = \"timeseries\"\nquery = \"q\"\nrange = \"30d\"\nstep = \"1m\"").contains("1000 points"));
        assert!(bad("title = \"x\"\nquery = \"q\"\nthresholds = [{value=10,level=\"warn\"},{value=5,level=\"ok\"}]").contains("ascending"));
        assert!(bad("title = \"x\"\nkind = \"gauge\"\nquery = \"q\"\nmin = 5\nmax = 5").contains("min < max"));
        assert!(bad("title = \"x\"\nurl = \"file:///etc/passwd\"").contains("http or https"));
        assert!(bad("title = \"x\"\nurl = \"http://h/x\"\npath = \"a.b\"").contains("JSON pointer"));
        assert!(bad("title = \"x\"\nsource = \"prometheus\"\nquery = \"q\"\nurl = \"http://h/x\"").contains("http_json"));
        assert!(bad("title = \"x\"\nurl = \"http://h/x\"\nsparkline = true").contains("prometheus"));
        assert!(bad("title = \"x\"\nkind = \"list\"\nrows = [{label = \"a\"}]").contains("row 'a'"));
        assert!(bad("title = \"x\"\nquery = \"q\"\nround = 11").contains("round"));

        let dup = parse(
            "[[section]]\ntitle = \"S\"\n[[section.panel]]\nid = \"a\"\ntitle = \"x\"\nquery = \"q\"\n[[section.panel]]\nid = \"a\"\ntitle = \"y\"\nquery = \"q\"",
        )
        .unwrap();
        assert!(validate(&dup).iter().any(|e| e.contains("duplicate id 'a'")));
    }

    #[tokio::test]
    async fn prometheus_stat_with_sparkline_transform_and_level() {
        let (prom, _) = fake_prometheus().await;
        let p = panel("title = \"Power\"\nunit = \"W\"\nquery = \"power\"\nround = 1\nsparkline = { range = \"1h\", step = \"20m\" }\nthresholds = [{value = 100, level = \"warn\"}, {value = 500, level = \"critical\"}]");
        let v = eval(&p, &prom).await;
        assert!(v.get("error").is_none(), "{v}");
        assert_eq!(v["kind"], "stat");
        assert_eq!(v["unit"], "W");
        assert_eq!(v["value"], json!(184.7));
        assert_eq!(v["level"], "warn");
        assert_eq!(v["sparkline"].as_array().unwrap().len(), 3);
        assert_eq!(v["sparkline"][1]["v"], json!(2.5));
        assert!(v["sparkline"][0]["t"].as_str().unwrap().ends_with('Z'));
        assert_eq!(v["refreshSeconds"], json!(15.0));
        assert!(v["updatedAt"].is_string());
    }

    #[tokio::test]
    async fn prometheus_gauge_scalar_timeseries_status_and_rows() {
        let (prom, _) = fake_prometheus().await;
        let g = eval(&panel("title = \"Share\"\nkind = \"gauge\"\nunit = \"%\"\nquery = \"share\"\nscale = 100\nround = 0"), &prom).await;
        assert_eq!((g["value"].clone(), g["min"].clone(), g["max"].clone()), (json!(63.0), json!(0.0), json!(100.0)));

        let s = eval(&panel("title = \"One\"\nquery = \"scalar(1)\""), &prom).await;
        assert_eq!(s["value"], json!(1.0), "{s}");

        let ts = eval(
            &panel("title = \"Solar vs lab\"\nkind = \"timeseries\"\nrange = \"3h\"\nstep = \"1h\"\nseries = [{name = \"Solar\", query = \"solar\"}, {name = \"Lab\", query = \"lab\"}, {name = \"Gone\", query = \"empty\"}]"),
            &prom,
        )
        .await;
        assert!(ts.get("error").is_none(), "one empty series doesn't fail the panel: {ts}");
        let series = ts["series"].as_array().unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[1]["name"], "Lab");
        assert_eq!(series[1]["points"][0]["v"], json!(15.0));

        let st = eval(&panel("title = \"Workers\"\nkind = \"status\"\nquery = \"workers\"\nlabel = \"worker\""), &prom).await;
        assert_eq!(
            st["rows"],
            json!([{"label": "a-cpu", "up": true, "value": 1.0}, {"label": "b-gpu", "up": false, "value": 0.0}])
        );

        let bar = eval(
            &panel("title = \"Temps\"\nkind = \"bar\"\nunit = \"°C\"\nmax = 100\nround = 0\nthresholds = [{value = 0, level = \"ok\"}, {value = 85, level = \"critical\"}]\nrows = [{label = \"A\", query = \"temp_a\"}, {label = \"Missing\", query = \"nothing\"}, {label = \"Hot\", query = \"temp_hot\"}]"),
            &prom,
        )
        .await;
        assert_eq!(
            bar["rows"],
            json!([{"label": "A", "value": 49.0, "level": "ok"}, {"label": "Hot", "value": 91.0, "level": "critical"}])
        );
        assert_eq!(bar["max"], json!(100.0));

        let list = eval(&panel("title = \"W\"\nkind = \"list\"\nquery = \"workers\""), &prom).await;
        assert_eq!(list["rows"][0]["label"], "a-cpu", "the default label skips __name__: {list}");
    }

    #[tokio::test]
    async fn http_json_panels() {
        let (prom, _) = fake_prometheus().await;
        let j = fake_json().await;
        let stat = eval(&panel(&format!("title = \"Power\"\nunit = \"W\"\nurl = \"{j}/doc\"\npath = \"/power/milliwatts\"\nscale = 0.001\nround = 1")), &prom).await;
        assert_eq!(stat["value"], json!(12.3), "{stat}");

        let text = eval(&panel(&format!("title = \"Note\"\nkind = \"text\"\nurl = \"{j}/doc\"\npath = \"/power/label\"")), &prom).await;
        assert_eq!(text["text"], "fine");

        let fixed = eval(&panel("title = \"Note\"\nkind = \"text\"\ntext = \"Backups run at 02:00\""), &prom).await;
        assert_eq!(fixed["text"], "Backups run at 02:00");

        let up = eval(&panel(&format!("title = \"API\"\nkind = \"status\"\nurl = \"{j}/doc\"\npath = \"/health\"")), &prom).await;
        assert_eq!(up["rows"], json!([{"label": "API", "up": true, "text": "OK"}]));

        let disks = eval(&panel(&format!("title = \"Disks\"\nkind = \"bar\"\nurl = \"{j}/doc\"\npath = \"/disks\"\nlabel = \"name\"\nvalue_key = \"used\"\nscale = 100")), &prom).await;
        assert_eq!(disks["rows"], json!([{"label": "sda", "value": 50.0}, {"label": "sdb", "value": 25.0}]));

        let fans = eval(&panel(&format!("title = \"Fans\"\nkind = \"list\"\nurl = \"{j}/doc\"\npath = \"/fans\"")), &prom).await;
        assert_eq!(fans["rows"], json!([{"label": "case", "value": 800.0}, {"label": "cpu", "value": 1200.0}]));

        let rows = eval(&panel(&format!("title = \"Mixed\"\nkind = \"list\"\nurl = \"{j}/doc\"\nrows = [{{label = \"mW\", path = \"/power/milliwatts\"}}, {{label = \"gone\", path = \"/nope\"}}]")), &prom).await;
        assert_eq!(rows["rows"], json!([{"label": "mW", "value": 12345.0}]));

        let ts = eval(
            &panel(&format!("title = \"H\"\nkind = \"timeseries\"\nurl = \"{j}/doc\"\nseries = [{{name = \"a\", path = \"/history\"}}, {{name = \"b\", path = \"/history_ms\"}}]")),
            &prom,
        )
        .await;
        assert!(ts.get("error").is_none(), "{ts}");
        assert_eq!(ts["series"][0]["points"][1]["v"], json!(2.5));
        assert_eq!(ts["series"][1]["points"][0]["t"], ts["series"][0]["points"][0]["t"], "ms timestamps read as seconds");
        assert_eq!(ts["series"][1]["points"][1]["v"], json!(4.0));
    }

    #[tokio::test]
    async fn every_failure_is_a_panel_error() {
        let (prom, _) = fake_prometheus().await;
        let j = fake_json().await;
        let err = |v: &Value| v["error"].as_str().unwrap_or_default().to_string();

        let none = eval(&panel("title = \"x\"\nquery = \"nothing\""), &prom).await;
        assert_eq!(err(&none), "no data");
        let boom = eval(&panel("title = \"x\"\nquery = \"boom\""), &prom).await;
        assert!(err(&boom).contains("parse error"), "{boom}");
        let unreachable = eval(&panel("title = \"x\"\nquery = \"power\""), "http://127.0.0.1:9").await;
        assert!(err(&unreachable).contains("Prometheus unreachable"), "{unreachable}");
        let slow = eval(&panel("title = \"x\"\nquery = \"slow\"\ntimeout = \"300ms\""), &prom).await;
        assert!(err(&slow).contains("timed out"), "{slow}");
        let http500 = eval(&panel(&format!("title = \"x\"\nurl = \"{j}/fail\"")), &prom).await;
        assert!(err(&http500).contains("HTTP 500"), "{http500}");
        let not_json = eval(&panel(&format!("title = \"x\"\nurl = \"{j}/text\"")), &prom).await;
        assert!(err(&not_json).contains("isn't JSON"), "{not_json}");
        let missing = eval(&panel(&format!("title = \"x\"\nurl = \"{j}/doc\"\npath = \"/nope\"")), &prom).await;
        assert!(err(&missing).contains("nothing at /nope"), "{missing}");
        let not_num = eval(&panel(&format!("title = \"x\"\nurl = \"{j}/doc\"\npath = \"/health\"")), &prom).await;
        assert!(err(&not_num).contains("expected a number"), "{not_num}");
        let invalid = eval(&panel("title = \"x\""), &prom).await;
        assert!(err(&invalid).starts_with("invalid panel"), "{invalid}");
        assert_eq!(invalid["title"], "x");

        // A status check that can't reach its target reads down, with the error.
        let down = eval(&panel(&format!("title = \"API\"\nkind = \"status\"\nurl = \"{j}/fail\"")), &prom).await;
        assert_eq!(down["rows"], json!([{"label": "API", "up": false}]));
        assert!(err(&down).contains("HTTP 500"));
    }

    #[tokio::test]
    async fn whole_dashboard_optional_sections_and_cache() {
        let (prom, calls) = fake_prometheus().await;
        let cfg = parse(
            r#"
            title = "Home"
            [[section]]
            title = "Power"
            [[section.panel]]
            title = "Power"
            query = "power"
            refresh = "1m"
            [[section.panel]]
            title = "Broken"
            query = "nothing"
            [[section]]
            title = "Solar"
            optional = true
            [[section.panel]]
            title = "Solar"
            query = "nothing"
            [[section]]
            title = "Gated"
            show_when = "nothing"
            [[section.panel]]
            title = "Power"
            query = "power"
            [[section]]
            title = "Open"
            show_when = "power"
            [[section.panel]]
            title = "Power"
            query = "power"
            "#,
        )
        .unwrap();
        let http = reqwest::Client::new();
        let src = Sources { http: &http, prometheus_url: &prom };
        let cache = Cache::default();
        let v = evaluate(&cfg, &src, Some(&cache)).await;
        assert_eq!(v["title"], "Home");
        let sections = v["sections"].as_array().unwrap();
        let titles: Vec<&str> = sections.iter().map(|s| s["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Power", "Open"], "no-data optional and show_when-gated sections are dropped: {v}");
        assert_eq!(sections[0]["panels"][0]["value"], json!(184.66));
        assert_eq!(sections[0]["panels"][1]["error"], "no data");
        assert_eq!(v["refreshSeconds"], json!(15.0));
        let before = calls.load(Ordering::SeqCst);
        let again = evaluate(&cfg, &src, Some(&cache)).await;
        assert_eq!(calls.load(Ordering::SeqCst), before, "fresh panels come from the cache");
        assert_eq!(again["sections"][0]["panels"][0]["value"], json!(184.66));
    }

    #[tokio::test]
    async fn loading_default_empty_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert!(matches!(load(&path, false), Loaded::Empty));
        assert!(matches!(load(&path, true), Loaded::Default(_)));
        std::fs::write(&path, "title = [").unwrap();
        let Loaded::Invalid(e) = load(&path, true) else { panic!("invalid TOML") };
        assert!(e.contains(FILE_NAME));
        let http = reqwest::Client::new();
        let src = Sources { http: &http, prometheus_url: "http://127.0.0.1:9" };
        let v = render(&Loaded::Invalid(e), &src, None).await;
        assert_eq!(v["sections"], json!([]));
        assert!(v["error"].as_str().unwrap().contains(FILE_NAME));
        let empty = render(&Loaded::Empty, &src, None).await;
        assert_eq!(empty["sections"], json!([]));
        assert_eq!(empty["title"], DEFAULT_TITLE);
        assert_eq!(empty["source"], "none");
        std::fs::write(&path, "title = \"Mine\"\n[[section]]\ntitle = \"S\"\n[[section.panel]]\ntitle = \"x\"\nquery = \"q\"").unwrap();
        assert!(matches!(load(&path, true), Loaded::Config(c) if c.title.as_deref() == Some("Mine")));
    }

    #[tokio::test]
    async fn default_dashboard_matches_the_old_lab_status() {
        let cfg = default_config(vec![("Server".into(), "temp_a".into())]);
        assert_eq!(validate(&cfg), Vec::<String>::new());
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(parse(&text).unwrap(), cfg, "the printed default reads back unchanged");
        let titles: Vec<&str> = cfg.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Power", "Workers", "Temperatures", "Solar"]);
        assert!(cfg.sections.iter().all(|s| s.optional));

        // Against an empty Prometheus every section drops out: an empty dashboard.
        let (prom, _) = fake_prometheus().await;
        let http = reqwest::Client::new();
        let v = render(&Loaded::Default(cfg), &Sources { http: &http, prometheus_url: &prom }, None).await;
        assert_eq!(v["source"], "default");
        assert_eq!(v["title"], "Lab");
        let shown: Vec<&str> = v["sections"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap()).collect();
        assert_eq!(shown, ["Temperatures"], "only the temperature row has data in the fake: {v}");
        assert_eq!(v["sections"][0]["panels"][0]["rows"], json!([{"label": "Server", "value": 49.0, "level": "ok"}]));
    }

    #[test]
    fn shipped_examples_are_valid() {
        for (name, text) in [
            ("dashboard.toml.example", include_str!("../../../../deploy/hive-serve/dashboard.toml.example")),
            ("dashboard-solar.toml.example", include_str!("../../../../deploy/hive-serve/dashboard-solar.toml.example")),
        ] {
            let cfg = parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(validate(&cfg), Vec::<String>::new(), "{name}");
            assert!(!cfg.sections.is_empty(), "{name}");
        }
    }
}
