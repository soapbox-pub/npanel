//! What staff should point a hostname at, for the dashboard to show them.
//!
//! Admins can spell it out in the `dns_targets` setting, but they rarely have
//! to: whatever the dashboard hostname resolves to is by definition an address
//! that reaches this server, since that is how they got to the dashboard.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

use crate::fields;
use crate::log::Logger;
use crate::settings::Settings;

/// How long a detected answer is reused before DNS is asked again.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long detection may take before it counts as having found nothing, so a
/// resolver that black-holes queries can't stall the dialog staff are staring at.
const DETECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DnsRecord {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub value: String,
}

/// The record that points a hostname at one target.
pub fn dns_record_for(target: &str) -> DnsRecord {
    let kind = match target.parse::<IpAddr>() {
        Ok(IpAddr::V4(_)) => "A",
        Ok(IpAddr::V6(_)) => "AAAA",
        Err(_) => "CNAME",
    };
    DnsRecord { kind, value: target.to_string() }
}

pub struct DnsTargets {
    dashboard_host: String,
    settings: Arc<Settings>,
    log: Logger,
    cache: Mutex<Option<(Vec<DnsRecord>, Instant)>>,
}

impl DnsTargets {
    pub fn new(dashboard_host: String, settings: Arc<Settings>, log: Logger) -> Self {
        Self { dashboard_host, settings, log, cache: Mutex::new(None) }
    }

    /// The records, and whether they came from `settings` or `dashboard_host`.
    pub async fn target(&self) -> (Vec<DnsRecord>, &'static str) {
        let configured = self.settings.get().dns_targets.clone();
        if !configured.is_empty() {
            return (configured.iter().map(|t| dns_record_for(t)).collect(), "settings");
        }
        (self.detect().await, "dashboard_host")
    }

    /// Addresses the dashboard hostname resolves to, cached including misses.
    async fn detect(&self) -> Vec<DnsRecord> {
        if let Some((records, at)) = self.cache.lock().as_ref() {
            if at.elapsed() < CACHE_TTL {
                return records.clone();
            }
        }

        let host = self.dashboard_host.clone();
        let lookup = async move {
            let mut addresses: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), 0)).await.map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
            // A records first, then AAAA, each once.
            addresses.sort_by_key(|a| a.is_ipv6());
            addresses.dedup();
            addresses
        };
        let addresses = tokio::time::timeout(DETECT_TIMEOUT, lookup).await.unwrap_or_default();

        if addresses.is_empty() {
            self.log.debug("dns_target_undetected", fields! { "dashboard_host" => self.dashboard_host.as_str() });
        }

        let records: Vec<DnsRecord> = addresses.iter().map(|a| dns_record_for(&a.to_string())).collect();
        *self.cache.lock() = Some((records.clone(), Instant::now()));
        records
    }
}
