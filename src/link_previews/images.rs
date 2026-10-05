//! The images sites' preview scripts drew, by version: in memory, on disk in
//! `DATA_DIR/preview-images/`, and still being drawn.
//!
//! A version names everything its image was drawn from, so an image never
//! goes stale under its name, and nothing has to invalidate one. Each tier
//! is bounded by what it holds, the least recently used going first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{oneshot, watch};

/// What images in memory may take up, and on disk.
const MEMORY_BYTES: usize = 64 << 20;
const DISK_BYTES: u64 = 512 << 20;

/// An image being drawn: `None` until it's done, then whether it came out.
type Drawing = watch::Receiver<Option<bool>>;

pub struct Images {
    dir: PathBuf,
    memory: Mutex<Tier<Bytes>>,
    disk: Mutex<Tier<u64>>,
    drawing: Mutex<HashMap<String, Drawing>>,
}

/// One tier's index: what it holds, by version, and its size in all.
struct Tier<T> {
    entries: lru::LruCache<String, T>,
    bytes: u64,
}

impl<T> Tier<T> {
    fn new() -> Tier<T> {
        Tier { entries: lru::LruCache::unbounded(), bytes: 0 }
    }
}

/// Whether a version is one npanel could have made: what's asked for by a
/// stranger is a file name.
pub fn valid(version: &str) -> bool {
    version.len() == 32 && version.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Images {
    /// Open the directory, and index what's there, oldest first. Blocking.
    pub fn open(dir: PathBuf) -> std::io::Result<Images> {
        std::fs::create_dir_all(&dir)?;
        let mut found = Vec::new();
        for shard in std::fs::read_dir(&dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(shard.path())? {
                let file = file?;
                let name = file.file_name().to_string_lossy().into_owned();
                let meta = file.metadata()?;
                if valid(&name) && meta.is_file() {
                    found.push((meta.modified().ok(), name, meta.len()));
                } else {
                    // Half-written, or not npanel's.
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
        found.sort();
        let mut disk = Tier::new();
        for (_, name, size) in found {
            disk.bytes += size;
            disk.entries.put(name, size);
        }
        let images = Images { dir, memory: Mutex::new(Tier::new()), disk: Mutex::new(disk), drawing: Mutex::new(HashMap::new()) };
        images.evict_disk();
        Ok(images)
    }

    fn path(&self, version: &str) -> PathBuf {
        self.dir.join(&version[..2]).join(version)
    }

    /// Whether an image of this version is drawn.
    pub fn has(&self, version: &str) -> bool {
        self.memory.lock().entries.contains(version) || self.disk.lock().entries.contains(version)
    }

    /// An image, if it's drawn.
    pub async fn get(&self, version: &str) -> Option<Bytes> {
        if !valid(version) {
            return None;
        }
        if let Some(image) = self.memory.lock().entries.get(version).cloned() {
            return Some(image);
        }
        self.disk.lock().entries.get(version)?;
        let path = self.path(version);
        let read = tokio::task::spawn_blocking(move || std::fs::read(path)).await.ok()?;
        match read {
            Ok(bytes) => {
                let image = Bytes::from(bytes);
                self.remember(version, image.clone());
                Some(image)
            }
            Err(_) => {
                self.forget_disk(version);
                None
            }
        }
    }

    /// An image, waiting up to `wait` for one being drawn.
    pub async fn wait(&self, version: &str, wait: Duration) -> Option<Bytes> {
        if let Some(image) = self.get(version).await {
            return Some(image);
        }
        let drawing = self.drawing.lock().get(version).cloned();
        let mut drawing = drawing?;
        let done = matches!(tokio::time::timeout(wait, drawing.wait_for(Option::is_some)).await, Ok(Ok(_)));
        if done {
            self.get(version).await
        } else {
            None
        }
    }

    /// Whether an image of this version is being drawn.
    pub fn is_drawing(&self, version: &str) -> bool {
        self.drawing.lock().contains_key(version)
    }

    /// An image a script is still drawing, kept once it's drawn.
    pub fn drawing(self: &Arc<Self>, version: String, image: oneshot::Receiver<Option<Vec<u8>>>) {
        if self.has(&version) {
            return;
        }
        let (done, watching) = watch::channel(None);
        {
            let mut drawing = self.drawing.lock();
            if drawing.contains_key(&version) {
                return;
            }
            drawing.insert(version.clone(), watching);
        }
        let images = self.clone();
        tokio::spawn(async move {
            let drawn = match image.await {
                Ok(Some(bytes)) => {
                    images.put(&version, bytes).await;
                    true
                }
                _ => false,
            };
            images.drawing.lock().remove(&version);
            let _ = done.send(Some(drawn));
        });
    }

    /// Keep an image, in memory and on disk.
    pub async fn put(&self, version: &str, bytes: Vec<u8>) {
        if !valid(version) || self.has(version) {
            return;
        }
        let image = Bytes::from(bytes);
        self.remember(version, image.clone());
        let path = self.path(version);
        let written = tokio::task::spawn_blocking(move || write(&path, &image)).await;
        if let Ok(Ok(size)) = written {
            {
                let mut disk = self.disk.lock();
                disk.bytes += size;
                if let Some(old) = disk.entries.put(version.to_string(), size) {
                    disk.bytes -= old;
                }
            }
            self.evict_disk();
        }
    }

    fn remember(&self, version: &str, image: Bytes) {
        let mut memory = self.memory.lock();
        memory.bytes += image.len() as u64;
        if let Some(old) = memory.entries.put(version.to_string(), image) {
            memory.bytes -= old.len() as u64;
        }
        while memory.bytes > MEMORY_BYTES as u64 {
            let Some((_, old)) = memory.entries.pop_lru() else { break };
            memory.bytes -= old.len() as u64;
        }
    }

    fn forget_disk(&self, version: &str) {
        let mut disk = self.disk.lock();
        if let Some(size) = disk.entries.pop(version) {
            disk.bytes -= size;
        }
    }

    fn evict_disk(&self) {
        let mut gone = Vec::new();
        {
            let mut disk = self.disk.lock();
            while disk.bytes > DISK_BYTES {
                let Some((version, size)) = disk.entries.pop_lru() else { break };
                disk.bytes -= size;
                gone.push(self.path(&version));
            }
        }
        for path in gone {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Forget every image. Blocking.
    pub fn clear(&self) {
        self.memory.lock().entries.clear();
        self.memory.lock().bytes = 0;
        let versions: Vec<String> = {
            let mut disk = self.disk.lock();
            disk.bytes = 0;
            let versions = disk.entries.iter().map(|(v, _)| v.clone()).collect();
            disk.entries.clear();
            versions
        };
        for version in versions {
            let _ = std::fs::remove_file(self.path(&version));
        }
    }
}

/// Write a file whole or not at all: to a temporary name, then renamed.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<u64> {
    let dir = path.parent().expect("a shard");
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(format!(".{}", crate::util::random_hex(8)));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn keeps_images_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let images = Arc::new(Images::open(dir.path().to_path_buf()).unwrap());
        assert!(!valid("../../etc/passwd") && !valid(&V.to_uppercase()) && valid(V));
        assert_eq!(images.get(V).await, None);

        let (tx, rx) = oneshot::channel();
        images.drawing(V.to_string(), rx);
        assert!(images.is_drawing(V));
        let waiting = {
            let images = images.clone();
            tokio::spawn(async move { images.wait(V, Duration::from_secs(5)).await })
        };
        tx.send(Some(b"image".to_vec())).unwrap();
        assert_eq!(waiting.await.unwrap().as_deref(), Some(&b"image"[..]));
        // Kept in memory at once, and on disk a moment later.
        for _ in 0..100 {
            if !images.is_drawing(V) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let reopened = Images::open(dir.path().to_path_buf()).unwrap();
        assert!(reopened.has(V));
        assert_eq!(reopened.get(V).await.as_deref(), Some(&b"image"[..]));
        reopened.clear();
        assert!(!reopened.has(V));
        assert!(Images::open(dir.path().to_path_buf()).unwrap().get(V).await.is_none());
    }
}
