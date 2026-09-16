use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use crate::engine::Match;

#[derive(Debug)]
pub(crate) struct TopMatches {
    limit: usize,
    directory: PathBuf,
    retained: BinaryHeap<Candidate>,
    next_id: u64,
}

#[derive(Debug)]
struct Candidate {
    probability: f64,
    source_path: PathBuf,
    start_byte: usize,
    stored_path: PathBuf,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // The worst retained match is the heap's maximum, ready for eviction.
        other
            .probability
            .total_cmp(&self.probability)
            .then_with(|| self.source_path.cmp(&other.source_path))
            .then_with(|| self.start_byte.cmp(&other.start_byte))
    }
}

impl TopMatches {
    pub(crate) fn new(limit: usize) -> io::Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let directory = std::env::temp_dir().join(format!(
                ".tsg-results-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        limit,
                        directory,
                        retained: BinaryHeap::new(),
                        next_id: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot create result spool",
        ))
    }

    pub(crate) fn consider(&mut self, item: &Match) -> io::Result<()> {
        let candidate = Candidate {
            probability: item.probability,
            source_path: item.unit.path.clone(),
            start_byte: item.unit.start_byte,
            stored_path: self.directory.join(self.next_id.to_string()),
        };
        if self.limit == 0
            || (self.retained.len() == self.limit
                && self
                    .retained
                    .peek()
                    .is_some_and(|worst| candidate >= *worst))
        {
            return Ok(());
        }
        // Persist just this bounded passage. Retaining K candidates never retains K bodies.
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate.stored_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, item)?;
        writer.flush()?;
        if self.retained.len() == self.limit {
            let removed = self.retained.pop().expect("full heap is nonempty");
            fs::remove_file(removed.stored_path)?;
        }
        self.retained.push(candidate);
        self.next_id += 1;
        Ok(())
    }

    pub(crate) fn drain_ranked(&mut self) -> impl Iterator<Item = io::Result<Match>> + '_ {
        // This Vec reuses the heap's bounded metadata allocation; bodies stay on disk.
        std::mem::take(&mut self.retained)
            .into_sorted_vec()
            .into_iter()
            .map(|candidate| {
                let reader = BufReader::new(File::open(&candidate.stored_path)?);
                let item = serde_json::from_reader(reader)?;
                fs::remove_file(candidate.stored_path)?;
                Ok(item)
            })
    }
}

impl Drop for TopMatches {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::Unit;

    fn item(name: &str, probability: f64) -> Match {
        Match {
            unit: Unit {
                path: name.into(),
                start_byte: 0,
                end_byte: 1024,
                start_line: 1,
                end_line: 1,
                kind: "text window".into(),
                headings: Vec::new(),
                target: "x".repeat(1024),
                context: String::new(),
            },
            probability,
        }
    }

    #[test]
    fn top_k_evicts_disk_bodies_and_streams_ranked_matches() {
        let mut top = TopMatches::new(2).unwrap();
        let directory = top.directory.clone();
        for (name, probability) in [("c", 0.8), ("d", 0.2), ("b", 0.9), ("a", 0.9)] {
            top.consider(&item(name, probability)).unwrap();
            assert!(top.retained.len() <= 2);
            assert!(fs::read_dir(&directory).unwrap().count() <= 2);
        }
        let mut results = top.drain_ranked();
        assert_eq!(
            results.next().unwrap().unwrap().unit.path,
            PathBuf::from("a")
        );
        assert_eq!(
            results.next().unwrap().unwrap().unit.path,
            PathBuf::from("b")
        );
        assert!(results.next().is_none());
        drop(results);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        drop(top);
        assert!(!directory.exists());
    }

    #[test]
    fn dropping_ranking_cleans_up_without_reading_retained_bodies() {
        let mut top = TopMatches::new(20).unwrap();
        top.consider(&item("file", 0.9)).unwrap();
        let directory = top.directory.clone();
        drop(top);
        assert!(!directory.exists());
    }
}
