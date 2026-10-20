//! `GET /v1/lab/status` — a best-effort snapshot of the home lab from
//! Prometheus (`HIVE_PROMETHEUS_URL`, default `http://127.0.0.1:9090`).
//!
//! Every query runs concurrently with a short timeout; a query that fails or
//! comes back empty just leaves its field out. Temperatures come from
//! `HIVE_LAB_TEMPS` (`Name=promql;Name=promql…`), defaulting to the hottest
//! sensor per exporter family.

use std::time::Duration;

use serde_json::{json, Map, Value};

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Default temperature series: (display name, PromQL).
const DEFAULT_TEMPS: &[(&str, &str)] = &[
    ("Desktop CPU", "max(lhm_cpu_temperature_celsius)"),
    ("Desktop GPU", "max(lhm_gpuamd_temperature_celsius)"),
    ("Server", "max(node_hwmon_temp_celsius)"),
    ("Jetson", "max(jetson_temp_celsius)"),
];

/// The configured temperature series.
pub fn temp_queries() -> Vec<(String, String)> {
    match crate::env_opt("HIVE_LAB_TEMPS") {
        Some(spec) => parse_temp_spec(&spec),
        None => DEFAULT_TEMPS.iter().map(|(n, q)| (n.to_string(), q.to_string())).collect(),
    }
}

fn parse_temp_spec(spec: &str) -> Vec<(String, String)> {
    spec.split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(n, q)| (n.trim().to_string(), q.trim().to_string()))
        .filter(|(n, q)| !n.is_empty() && !q.is_empty())
        .collect()
}

/// One instant-vector sample: (labels, value).
type Sample = (Map<String, Value>, f64);

/// Run one instant query. `None` on any failure.
async fn query(client: &reqwest::Client, base: &str, promql: &str) -> Option<Vec<Sample>> {
    let url = format!("{}/api/v1/query", base.trim_end_matches('/'));
    let resp = client.get(url).query(&[("query", promql)]).timeout(QUERY_TIMEOUT).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    parse_vector(&v)
}

/// Parse a Prometheus instant-query response body into samples.
pub fn parse_vector(v: &Value) -> Option<Vec<Sample>> {
    if v.get("status").and_then(Value::as_str) != Some("success") {
        return None;
    }
    let rows = v.pointer("/data/result")?.as_array()?;
    Some(
        rows.iter()
            .filter_map(|r| {
                let labels = r.get("metric").and_then(Value::as_object).cloned().unwrap_or_default();
                let val = r.pointer("/value/1")?.as_str()?.parse::<f64>().ok()?;
                val.is_finite().then_some((labels, val))
            })
            .collect(),
    )
}

fn scalar(samples: Option<Vec<Sample>>) -> Option<f64> {
    samples.and_then(|s| s.first().map(|(_, v)| *v))
}

/// Assemble the status object from the query results.
pub fn assemble(
    workers: Option<Vec<Sample>>,
    power: Option<f64>,
    cost: Option<f64>,
    rate: Option<f64>,
    temps: Vec<(String, Option<f64>)>,
) -> Value {
    let mut workers: Vec<Value> = workers
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(labels, v)| {
            let name = labels.get("worker").and_then(Value::as_str)?.to_string();
            Some(json!({ "name": name, "up": v >= 1.0 }))
        })
        .collect();
    workers.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let temps: Vec<Value> = temps
        .into_iter()
        .filter_map(|(name, v)| v.map(|c| json!({ "name": name, "celsius": (c * 10.0).round() / 10.0 })))
        .collect();
    let mut out = json!({
        "workers": workers,
        "temps": temps,
        "updatedAt": super::api::iso(hive_core::Timestamp::now()),
    });
    let o = out.as_object_mut().expect("object");
    if let Some(w) = power {
        o.insert("powerWatts".into(), json!((w * 10.0).round() / 10.0));
    }
    if let Some(c) = cost {
        o.insert("costPerHour".into(), json!(c));
    }
    if let Some(r) = rate {
        o.insert("ratePerKwh".into(), json!(r));
    }
    out
}

/// Query Prometheus and build the `/v1/lab/status` body.
pub async fn status(client: &reqwest::Client, base: &str) -> Value {
    let temp_qs = temp_queries();
    let temps_fut = futures_util::future::join_all(temp_qs.iter().map(|(_, q)| query(client, base, q)));
    let (workers, power, cost, rate, temps) = tokio::join!(
        query(client, base, "rdnagw_worker_up"),
        query(client, base, "lab:power_watts:sum"),
        query(client, base, "lab:cost_usd_per_hour"),
        query(client, base, "lab:electric_rate_usd_per_kwh"),
        temps_fut,
    );
    let temps = temp_qs
        .into_iter()
        .zip(temps)
        .map(|((name, _), r)| (name, scalar(r)))
        .collect();
    assemble(workers, scalar(power), scalar(cost), scalar(rate), temps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_assembles_with_missing_fields_omitted() {
        let body = json!({"status":"success","data":{"resultType":"vector","result":[
            {"metric":{"worker":"server-cpu"},"value":[1.0,"1"]},
            {"metric":{"worker":"desktop-6900xt"},"value":[1.0,"0"]}
        ]}});
        let workers = parse_vector(&body);
        let out = assemble(workers, Some(184.61), None, Some(0.25), vec![("Server".into(), Some(49.25)), ("Jetson".into(), None)]);
        assert_eq!(out["workers"][0], json!({"name":"desktop-6900xt","up":false}));
        assert_eq!(out["workers"][1], json!({"name":"server-cpu","up":true}));
        assert_eq!(out["powerWatts"], json!(184.6));
        assert!(out.get("costPerHour").is_none());
        assert_eq!(out["ratePerKwh"], json!(0.25));
        assert_eq!(out["temps"], json!([{"name":"Server","celsius":49.3}]));
        assert!(out["updatedAt"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn failed_query_is_none() {
        assert!(parse_vector(&json!({"status":"error"})).is_none());
    }

    #[test]
    fn temp_spec_parses() {
        let v = parse_temp_spec("A=max(x); B = y ;bad;=z");
        assert_eq!(v, vec![("A".into(), "max(x)".into()), ("B".into(), "y".into())]);
    }

    #[tokio::test]
    async fn unreachable_prometheus_yields_empty_status() {
        let client = reqwest::Client::new();
        let out = status(&client, "http://127.0.0.1:9").await;
        assert_eq!(out["workers"], json!([]));
        assert_eq!(out["temps"], json!([]));
        assert!(out.get("powerWatts").is_none());
    }
}
