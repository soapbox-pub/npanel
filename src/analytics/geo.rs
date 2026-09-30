//! Turns an address into a country, region and city, with DB-IP's free City
//! Lite database.
//!
//! DB-IP publishes a new one at the start of each month under CC BY 4.0, which
//! asks for attribution — the dashboard gives it wherever locations are shown.
//! It is downloaded into `DATA_DIR/geo` the first time any site has analytics
//! on, and replaced once a month; until the first download lands, visits are
//! counted without a location rather than waited on.
//!
//! The file is memory-mapped: a lookup touches a few kilobytes of a hundred
//! megabytes, and the kernel keeps the pages that are used and can reclaim the
//! rest. The address itself is used for the lookup and nothing else.

use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Datelike;
use parking_lot::RwLock;
use serde::Deserialize;

use crate::fields;
use crate::log::{with_err, Logger};

/// Where a visitor was, as far as their address says. Empty means unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    /// ISO 3166-1 alpha-2.
    pub country: String,
    /// English name of the first-level subdivision.
    pub region: String,
    /// English name of the city.
    pub city: String,
}

#[derive(Deserialize)]
struct Names<'a> {
    #[serde(borrow)]
    en: Option<&'a str>,
}

#[derive(Deserialize)]
struct Named<'a> {
    #[serde(borrow)]
    names: Option<Names<'a>>,
}

#[derive(Deserialize)]
struct Country<'a> {
    iso_code: Option<&'a str>,
}

#[derive(Deserialize)]
struct CityRecord<'a> {
    #[serde(borrow)]
    country: Option<Country<'a>>,
    #[serde(borrow)]
    subdivisions: Option<Vec<Named<'a>>>,
    #[serde(borrow)]
    city: Option<Named<'a>>,
}

/// Codes that name no country: unknown, and Tor, which DB-IP files as a place.
const NOT_COUNTRIES: [&str; 3] = ["ZZ", "XX", "T1"];

const FILE: &str = "dbip-city-lite.mmdb";

/// How long to wait between checks for a newer database.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

type Reader = maxminddb::Reader<memmap2::Mmap>;

#[derive(Clone)]
pub struct Geo {
    inner: Arc<Inner>,
}

struct Inner {
    dir: PathBuf,
    log: Logger,
    reader: RwLock<Option<Reader>>,
    updating: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Geo {
    pub fn new(dir: &Path, log: Logger) -> Geo {
        Geo { inner: Arc::new(Inner { dir: dir.to_path_buf(), log, reader: RwLock::new(None), updating: Default::default() }) }
    }

    /// Load whatever database is already on disk.
    pub fn load(&self) {
        match Reader::open_mmap(self.inner.dir.join(FILE)) {
            Ok(reader) => {
                let built = chrono::DateTime::from_timestamp(reader.metadata.build_epoch as i64, 0).unwrap_or_default();
                self.inner.log.info("geo_loaded", fields! { "built" => built.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) });
                *self.inner.reader.write() = Some(reader);
            }
            Err(maxminddb::MaxMindDbError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => self.inner.log.warn("geo_load_failed", crate::log::err_fields(error)),
        }
    }

    /// Keep the database current, from now on, on `runtime`. Idempotent.
    pub fn keep_updated(&self, runtime: &tokio::runtime::Handle) {
        let mut updating = self.inner.updating.lock();
        if updating.is_some() {
            return;
        }
        let geo = self.clone();
        *updating = Some(runtime.spawn(async move {
            loop {
                geo.update().await;
                tokio::time::sleep(CHECK_EVERY).await;
            }
        }));
    }

    pub fn stop(&self) {
        if let Some(task) = self.inner.updating.lock().take() {
            task.abort();
        }
    }

    pub fn lookup(&self, ip: &str) -> Location {
        let Ok(address) = ip.split('%').next().unwrap_or("").parse::<IpAddr>() else {
            return Location::default();
        };
        let reader = self.inner.reader.read();
        let Some(reader) = reader.as_ref() else {
            return Location::default();
        };
        let Ok(Some(record)) = reader.lookup::<CityRecord>(address) else {
            return Location::default();
        };

        let country = record.country.and_then(|c| c.iso_code).unwrap_or("");
        if NOT_COUNTRIES.contains(&country) {
            return Location::default();
        }
        let name = |named: Option<&Named>| named.and_then(|n| n.names.as_ref()).and_then(|n| n.en).unwrap_or("").to_string();

        Location {
            country: country.to_string(),
            region: name(record.subdivisions.as_ref().and_then(|s| s.first())),
            city: name(record.city.as_ref()),
        }
    }

    /// Download this month's database if the one on disk is older. Judged by
    /// when DB-IP built it rather than when it was downloaded.
    async fn update(&self) {
        let now = chrono::Utc::now();
        let month = now.format("%Y-%m").to_string();
        let previous = (now.date_naive().with_day(1).unwrap_or_default() - chrono::Days::new(1)).format("%Y-%m").to_string();
        let have = self.inner.reader.read().as_ref().map(|r| {
            chrono::DateTime::from_timestamp(r.metadata.build_epoch as i64, 0).unwrap_or_default().format("%Y-%m").to_string()
        });

        // This month's may not be published yet in its first days; last
        // month's is then the newest there is, and better than none.
        for candidate in [month, previous] {
            if have.as_ref().is_some_and(|have| *have >= candidate) {
                return;
            }
            let url = format!("https://download.db-ip.com/free/dbip-city-lite-{candidate}.mmdb.gz");
            match self.download(&url).await {
                Ok(true) => {
                    self.inner.log.info("geo_downloaded", fields! { "month" => candidate });
                    let geo = self.clone();
                    let _ = tokio::task::spawn_blocking(move || geo.load()).await;
                    return;
                }
                Ok(false) => continue,
                Err(error) => {
                    self.inner.log.warn("geo_download_failed", with_err(fields! { "url" => url }, error));
                    return;
                }
            }
        }
    }

    /// Fetch and unpack one month's file. `Ok(false)` when it isn't published.
    ///
    /// The body streams to a blocking task that decompresses it to disk, so
    /// neither the unpacking nor the writes happen on a runtime thread, and
    /// the file only takes its real name once it is whole.
    async fn download(&self, url: &str) -> anyhow::Result<bool> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(10 * 60)).build()?;
        let mut response = client.get(url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status().as_u16());
        }

        let dir = self.inner.dir.clone();
        let tmp = dir.join(format!("{FILE}.tmp"));
        let (chunks, mut received) = tokio::sync::mpsc::channel::<bytes::Bytes>(16);
        let writer = tokio::task::spawn_blocking({
            let tmp = tmp.clone();
            move || {
                std::fs::create_dir_all(&dir)?;
                let mut out = flate2::write::GzDecoder::new(std::io::BufWriter::new(std::fs::File::create(&tmp)?));
                while let Some(chunk) = received.blocking_recv() {
                    out.write_all(&chunk)?;
                }
                out.finish()?.flush()?;
                anyhow::Ok(())
            }
        });

        let sent = async {
            while let Some(chunk) = response.chunk().await? {
                // The writer only goes away having failed, which it reports.
                if chunks.send(chunk).await.is_err() {
                    break;
                }
            }
            anyhow::Ok(())
        }
        .await;
        drop(chunks);
        let written = writer.await?;

        let target = self.inner.dir.join(FILE);
        let result = match sent.and(written) {
            Ok(()) => tokio::fs::rename(&tmp, target).await.map_err(Into::into),
            Err(error) => Err(error),
        };
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        result.map(|()| true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_without_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let geo = Geo::new(dir.path(), Logger::new(crate::log::LogLevel::Error));
        geo.load();
        assert_eq!(geo.lookup("8.8.8.8"), Location::default());
        assert_eq!(geo.lookup("not an ip"), Location::default());
    }
}
