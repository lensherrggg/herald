use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 200;
const HALF_LIFE_DAYS: f64 = 7.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    path: String,
    count: u32,
    last_used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HistoryFile {
    #[serde(default)]
    entries: Vec<HistoryEntry>,
}

/// A small zoxide-style directory history. Visits are recorded per directory
/// and ranked by frequency with an exponential recency decay.
#[derive(Debug, Default)]
pub struct DirHistory {
    file: Option<PathBuf>,
    entries: Vec<HistoryEntry>,
}

impl DirHistory {
    pub fn load(file: PathBuf) -> Self {
        let entries = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<HistoryFile>(&text).ok())
            .map(|history| history.entries)
            .unwrap_or_default();
        Self {
            file: Some(file),
            entries,
        }
    }

    pub fn record(&mut self, dir: &Path) {
        self.record_at(dir, now());
    }

    fn record_at(&mut self, dir: &Path, now: u64) {
        if !dir.is_absolute() {
            return;
        }
        let path = dir.to_string_lossy().to_string();
        if path.is_empty() {
            return;
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| same_path(&entry.path, &path))
        {
            entry.count = entry.count.saturating_add(1);
            entry.last_used = now;
        } else {
            self.entries.push(HistoryEntry {
                path,
                count: 1,
                last_used: now,
            });
        }
        self.trim(now);
        self.save();
    }

    /// Rank matching directories by frecency. Every whitespace-separated term
    /// must appear as a case-insensitive subsequence of the full path, which
    /// lets `dwn` find `Downloads` while keeping multi-word queries narrow.
    pub fn search(&self, query: &str, limit: usize, exclude: Option<&Path>) -> Vec<PathBuf> {
        let now = now();
        let exclude = exclude.map(|path| path.to_string_lossy().to_string());
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .collect();
        let mut scored: Vec<(f64, &HistoryEntry)> = self
            .entries
            .iter()
            .filter(|entry| {
                !exclude
                    .as_deref()
                    .is_some_and(|skip| same_path(skip, &entry.path))
            })
            .filter(|entry| {
                let path = entry.path.to_lowercase();
                terms.iter().all(|term| subsequence(&path, term))
            })
            .map(|entry| (score(entry, now), entry))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
        let mut results = Vec::new();
        for (_, entry) in scored {
            let path = PathBuf::from(&entry.path);
            if path.is_dir() {
                results.push(path);
                if results.len() >= limit {
                    break;
                }
            }
        }
        results
    }

    fn trim(&mut self, now: u64) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        self.entries.sort_by(|a, b| {
            score(b, now)
                .total_cmp(&score(a, now))
                .then_with(|| a.path.cmp(&b.path))
        });
        self.entries.truncate(MAX_ENTRIES);
    }

    fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(&HistoryFile {
            entries: self.entries.clone(),
        }) {
            let _ = fs::write(file, text);
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn score(entry: &HistoryEntry, now: u64) -> f64 {
    let age_days = now.saturating_sub(entry.last_used) as f64 / 86_400.0;
    let decay = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
    entry.count as f64 * (0.05 + decay)
}

fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|want| chars.any(|ch| ch == want))
}

fn same_path(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_frequently_and_recently_used_directories() {
        let temp = tempfile::tempdir().unwrap();
        let alpha = temp.path().join("alpha");
        let beta = temp.path().join("beta");
        let downloads = temp.path().join("Downloads");
        for path in [&alpha, &beta, &downloads] {
            fs::create_dir(path).unwrap();
        }
        let mut history = DirHistory::default();
        history.record_at(&alpha, 1_000_000);
        history.record_at(&alpha, 1_000_000);
        history.record_at(&beta, 1_000_000);
        history.record_at(&downloads, 1_000_000);

        let all = history.search("", 10, Some(temp.path()));
        assert_eq!(
            all,
            vec![alpha.clone(), downloads.clone(), beta.clone()],
            "alpha wins by frequency; the tie sorts by path"
        );

        let fuzzy = history.search("dwn", 10, None);
        assert_eq!(fuzzy, vec![downloads.clone()]);
        let excluded = history.search("dwn", 10, Some(&downloads));
        assert!(excluded.is_empty());
    }

    #[test]
    fn persists_history_and_ignores_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("dir_history.json");
        let keep = temp.path().join("keep");
        fs::create_dir(&keep).unwrap();

        {
            let mut history = DirHistory::load(file.clone());
            history.record(&keep);
            history.record(Path::new("relative/no"));
            assert!(file.is_file());
        }
        let loaded = DirHistory::load(file);
        assert_eq!(loaded.search("", 10, None), vec![keep]);
    }
}
