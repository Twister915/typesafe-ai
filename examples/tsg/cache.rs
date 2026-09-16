use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Default)]
pub(crate) struct Cache {
    directory: Option<PathBuf>,
}

impl Cache {
    pub(crate) fn new(directory: Option<PathBuf>) -> Result<Self, String> {
        if let Some(path) = &directory
            && path.exists()
            && !path.is_dir()
        {
            return Err(format!(
                "cache {} is a file; streaming caches require a directory (choose a new path)",
                path.display()
            ));
        }
        Ok(Self { directory })
    }

    pub(crate) async fn get(&self, key: &str) -> Result<Option<f64>, String> {
        let Some(directory) = &self.directory else {
            return Ok(None);
        };
        let path = entry_path(directory, key);
        let file = match tokio::fs::File::open(&path).await {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read cache {}: {error}", path.display())),
        };
        let mut bytes = Vec::with_capacity(128);
        file.take(129)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("cannot read cache {}: {error}", path.display()))?;
        if bytes.len() > 128 {
            return Err(format!("cache entry {} exceeds 128 bytes", path.display()));
        }
        let value: f64 = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid cache {}: {error}", path.display()))?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!("invalid probability in cache {}", path.display()));
        }
        Ok(Some(value))
    }

    pub(crate) async fn insert(&self, key: &str, value: f64) -> Result<(), String> {
        let Some(directory) = &self.directory else {
            return Ok(());
        };
        let path = entry_path(directory, key);
        let parent = path.parent().expect("cache entries have a shard directory");
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("cannot create cache {}: {error}", parent.display()))?;
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let mut temporary = None;
        for _ in 0..100 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".{}.{}.tmp", std::process::id(), id));
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
                .await
            {
                Ok(file) => {
                    temporary = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("cannot create cache temporary file: {error}")),
            }
        }
        let (temporary, mut file) = temporary.ok_or("cannot allocate cache temporary file")?;
        let temporary = TemporaryEntry(temporary);
        let result = async {
            file.write_all(value.to_string().as_bytes()).await?;
            file.flush().await?;
            drop(file);
            tokio::fs::rename(&temporary.0, &path).await
        }
        .await;
        if let Err(error) = result {
            return Err(format!("cannot write cache {}: {error}", path.display()));
        }
        Ok(())
    }
}

struct TemporaryEntry(PathBuf);

impl Drop for TemporaryEntry {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn entry_path(directory: &Path, key: &str) -> PathBuf {
    directory
        .join(&key[..2])
        .join(format!("{}.json", &key[2..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestDirectory;

    #[tokio::test]
    async fn entries_are_loaded_individually_and_oversized_entries_are_rejected() {
        let directory = TestDirectory::new();
        let cache = Cache::new(Some(directory.0.clone())).unwrap();
        let key = "a".repeat(64);
        assert_eq!(cache.get(&key).await.unwrap(), None);
        cache.insert(&key, 0.8).await.unwrap();
        assert_eq!(cache.get(&key).await.unwrap(), Some(0.8));
        std::fs::write(entry_path(&directory.0, &key), "0".repeat(10000)).unwrap();
        assert!(
            cache
                .get(&key)
                .await
                .unwrap_err()
                .contains("exceeds 128 bytes")
        );
    }

    #[test]
    fn legacy_monolithic_cache_requires_a_new_directory() {
        let directory = TestDirectory::new();
        let path = directory.0.join("old.json");
        std::fs::write(&path, "{}").unwrap();
        assert!(
            Cache::new(Some(path))
                .unwrap_err()
                .contains("streaming caches require a directory")
        );
    }
}
