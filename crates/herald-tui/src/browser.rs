use std::fs;
use std::path::{Path, PathBuf};

use pinyin::ToPinyin;

use crate::dir_history::DirHistory;

const JUMP_LIMIT: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// In-progress `z` jump: a fuzzy query over the directory history and the
/// ranked candidates it produced.
#[derive(Debug, Default)]
pub struct JumpState {
    pub query: String,
    pub cursor: usize,
    pub matches: Vec<PathBuf>,
    pub selected: usize,
}

#[derive(Debug)]
pub struct Browser {
    pub dir: PathBuf,
    all_entries: Vec<BrowserEntry>,
    pub entries: Vec<BrowserEntry>,
    pub selected: usize,
    pub filter: String,
    pub filter_cursor: usize,
    pub filtering: bool,
    pub filename: String,
    pub filename_cursor: usize,
    pub filename_focus: bool,
    pub uidl: String,
    pub attachment_index: usize,
    pub original_name: String,
    pub note: Option<String>,
    pub history: DirHistory,
    pub jump: Option<JumpState>,
}

impl Browser {
    pub fn new(
        start: PathBuf,
        uidl: String,
        attachment_index: usize,
        original_name: String,
    ) -> Self {
        let (dir, note) = resolve_start(start);
        let mut browser = Self {
            dir,
            all_entries: Vec::new(),
            entries: Vec::new(),
            selected: 0,
            filter: String::new(),
            filter_cursor: 0,
            filtering: false,
            filename: original_name.clone(),
            filename_cursor: original_name.len(),
            filename_focus: false,
            uidl,
            attachment_index,
            original_name,
            note,
            history: DirHistory::default(),
            jump: None,
        };
        browser.refresh();
        browser
    }

    /// Load the persistent visit history and count the starting directory as
    /// a visit, so the first `z` already has a candidate.
    pub fn attach_history(&mut self, mut history: DirHistory) {
        history.record(&self.dir);
        self.history = history;
    }

    pub fn refresh(&mut self) {
        match read_entries(&self.dir) {
            Ok(entries) => self.all_entries = entries,
            Err(err) => {
                let fallback = dirs_home().unwrap_or_else(|| PathBuf::from("."));
                if fallback != self.dir {
                    self.note = Some(format!(
                        "无法读取 {}（{err}），已转到 {}",
                        self.dir.display(),
                        fallback.display()
                    ));
                    self.dir = fallback;
                    self.all_entries = read_entries(&self.dir).unwrap_or_default();
                    self.selected = 0;
                } else {
                    self.all_entries.clear();
                    self.selected = 0;
                    self.note = Some(format!("无法读取目录：{err}"));
                }
            }
        }
        self.apply_filter();
    }

    /// Rebuild the visible list from the current filter. The parent row is
    /// always visible so navigation never traps the user. Queries containing
    /// a path separator match the absolute path instead of just the name.
    /// ASCII queries also match Chinese names by pinyin initials (`xm` → 项目).
    pub fn apply_filter(&mut self) {
        let previous = self
            .entries
            .get(self.selected)
            .map(|entry| entry.path.clone());
        let query = self.filter.to_lowercase();
        let by_path = query.contains('/') || (cfg!(windows) && query.contains('\\'));
        let ascii_query = !query.is_empty() && query.is_ascii();
        let compact_query: String = query
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect();
        self.entries = self
            .all_entries
            .iter()
            .filter(|entry| {
                if entry.name == ".." || query.is_empty() {
                    return true;
                }
                let text = if by_path {
                    entry.path.to_string_lossy().to_lowercase()
                } else {
                    entry.name.to_lowercase()
                };
                if text.contains(&query) {
                    return true;
                }
                if !ascii_query {
                    return false;
                }
                let Some((skeleton, compact)) = pinyin_keys(&text) else {
                    return false;
                };
                skeleton.contains(&query)
                    || (!by_path && !compact_query.is_empty() && compact.contains(&compact_query))
            })
            .cloned()
            .collect();
        self.selected = previous
            .and_then(|path| self.entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
    }

    /// An existing absolute directory typed into the filter box. Enter jumps
    /// straight there, which makes `/usr/local` (macOS/Linux) and `C:\Tools`
    /// (Windows) behave like native paths.
    pub fn filter_jump_target(&self) -> Option<PathBuf> {
        let query = self.filter.trim();
        if query.is_empty() {
            return None;
        }
        let path = PathBuf::from(query);
        (path.is_absolute() && path.is_dir()).then_some(path)
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_cursor = 0;
        self.filtering = false;
    }

    pub fn filter_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.name != "..")
            .count()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.entries.len() - 1;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, last as isize) as usize;
    }

    /// Enter on a directory descends into it; on a file it copies the name
    /// into the filename field so the user can reuse/edit it.
    pub fn enter(&mut self) {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        if entry.is_dir {
            self.jump_to_path(entry.path);
        } else {
            self.filename = entry.name;
            self.filename_cursor = self.filename.len();
            self.filename_focus = true;
        }
    }

    /// Move to the parent directory. Returns false at the filesystem root.
    pub fn go_up(&mut self) -> bool {
        let Some(parent) = self.dir.parent() else {
            return false;
        };
        let parent = parent.to_path_buf();
        if parent == self.dir {
            return false;
        }
        self.jump_to_path(parent)
    }

    /// Switch to `path` and remember the visit. Used by Enter, the parent key
    /// and every jump, so history reflects how the user actually moved.
    pub fn jump_to_path(&mut self, path: PathBuf) -> bool {
        if !path.is_dir() {
            return false;
        }
        self.dir = path;
        self.selected = 0;
        self.note = None;
        self.clear_filter();
        self.refresh();
        self.history.record(&self.dir);
        true
    }

    pub fn open_jump(&mut self) {
        self.jump = Some(JumpState::default());
        self.update_jump();
    }

    /// Re-rank candidates after the query changed; the top hit becomes the
    /// selection so Enter jumps to the best match straight away.
    pub fn update_jump(&mut self) {
        if let Some(jump) = self.jump.as_mut() {
            jump.matches = self
                .history
                .search(&jump.query, JUMP_LIMIT, Some(&self.dir));
            jump.selected = 0;
        }
    }

    pub fn move_jump(&mut self, delta: isize) {
        let Some(jump) = self.jump.as_mut() else {
            return;
        };
        if jump.matches.is_empty() {
            jump.selected = 0;
            return;
        }
        let last = jump.matches.len() as isize - 1;
        jump.selected = (jump.selected as isize + delta).clamp(0, last) as usize;
    }

    pub fn cancel_jump(&mut self) {
        self.jump = None;
    }

    /// The highlighted candidate, or an existing absolute path typed in full.
    pub fn jump_target(&self) -> Option<PathBuf> {
        let jump = self.jump.as_ref()?;
        if let Some(path) = jump.matches.get(jump.selected) {
            return Some(path.clone());
        }
        let typed = PathBuf::from(jump.query.trim());
        (typed.is_absolute() && typed.is_dir()).then_some(typed)
    }

    pub fn jump_to_selected(&mut self) -> Option<PathBuf> {
        let target = self.jump_target()?;
        if !self.jump_to_path(target.clone()) {
            return None;
        }
        self.jump = None;
        Some(target)
    }

    pub fn focus_filename(&mut self) {
        self.filename_cursor = self.filename.len();
        self.filename_focus = true;
    }

    pub fn unfocus_filename(&mut self) {
        self.filename_focus = false;
    }

    pub fn dir_target(&self) -> PathBuf {
        self.dir.clone()
    }

    pub fn file_target(&self) -> Option<PathBuf> {
        let name = self.filename.trim();
        if !valid_file_name(name) {
            return None;
        }
        Some(self.dir.join(name))
    }

    pub fn create_dir(&mut self, name: &str) -> std::io::Result<()> {
        let name = name.trim();
        if !valid_file_name(name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "目录名不能为空，且不能包含路径分隔符",
            ));
        }
        let path = self.dir.join(name);
        fs::create_dir(&path)?;
        self.clear_filter();
        self.refresh();
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            self.selected = index;
        }
        Ok(())
    }

    pub fn entry_label(entry: &BrowserEntry) -> String {
        if entry.is_dir {
            if entry.name == ".." {
                "..（上一级）".to_string()
            } else if entry.path.parent().is_none() {
                entry.name.clone()
            } else {
                format!("{}/", entry.name)
            }
        } else {
            entry.name.clone()
        }
    }
}

/// Pinyin initials for every Chinese character, with other characters kept
/// lowercased. The first key preserves separators for absolute-path filters;
/// the second drops punctuation so `xmgl` can match `项目 管理`. Returns
/// `None` when the text has no Chinese characters with pinyin data.
fn pinyin_keys(text: &str) -> Option<(String, String)> {
    let mut skeleton = String::with_capacity(text.len());
    let mut compact = String::with_capacity(text.len());
    let mut translated = false;
    for ch in text.chars() {
        match ch.to_pinyin() {
            Some(pinyin) => {
                let initial = pinyin.first_letter();
                skeleton.push_str(initial);
                compact.push_str(initial);
                translated = true;
            }
            None => {
                for lower in ch.to_lowercase() {
                    skeleton.push(lower);
                    if lower.is_ascii_alphanumeric() {
                        compact.push(lower);
                    }
                }
            }
        }
    }
    translated.then_some((skeleton, compact))
}

fn read_entries(dir: &Path) -> std::io::Result<Vec<BrowserEntry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Some(parent) = dir.parent() {
        dirs.push(BrowserEntry {
            name: "..".to_string(),
            path: parent.to_path_buf(),
            is_dir: true,
        });
    } else {
        dirs.extend(drive_entries(dir));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = path.is_dir();
        let item = BrowserEntry { name, path, is_dir };
        if is_dir {
            dirs.push(item);
        } else {
            files.push(item);
        }
    }
    dirs.sort_by(|a, b| {
        if a.name == ".." {
            return std::cmp::Ordering::Less;
        }
        if b.name == ".." {
            return std::cmp::Ordering::Greater;
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });
    files.sort_by_key(|entry| entry.name.to_lowercase());
    dirs.extend(files);
    Ok(dirs)
}

#[cfg(windows)]
fn drive_entries(current: &Path) -> Vec<BrowserEntry> {
    // SAFETY: GetLogicalDrives takes no arguments and only returns a bitmask.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    drives_from_mask(mask, current)
}

#[cfg(windows)]
fn drives_from_mask(mask: u32, current: &Path) -> Vec<BrowserEntry> {
    (0u8..26)
        .filter(|index| mask & (1u32 << index) != 0)
        .filter_map(|index| {
            let name = format!("{}:\\", (b'A' + index) as char);
            let path = PathBuf::from(&name);
            (!current.starts_with(&path)).then_some(BrowserEntry {
                name,
                path,
                is_dir: true,
            })
        })
        .collect()
}

#[cfg(not(windows))]
fn drive_entries(_current: &Path) -> Vec<BrowserEntry> {
    Vec::new()
}

fn resolve_start(start: PathBuf) -> (PathBuf, Option<String>) {
    if start.is_dir() {
        return (start, None);
    }
    let mut note = Some(format!("起始目录不存在：{}", start.display()));
    let home = dirs_home();
    if let Some(home) = home {
        if home.is_dir() {
            note = Some(format!(
                "起始目录不存在：{}，已改用 {}",
                start.display(),
                home.display()
            ));
            return (home, note);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        return (cwd, note);
    }
    (PathBuf::from("."), note)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .filter(|path| path.is_dir())
}

/// Strip any path components and characters that are invalid on Windows or
/// that could escape the chosen directory.
pub fn sanitize_filename(name: &str, index: usize) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('.');
    let cleaned: String = base
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        format!("attachment-{}.bin", index + 1)
    } else {
        cleaned.to_string()
    }
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_parent_first_then_dirs_then_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("zdir")).unwrap();
        fs::create_dir(temp.path().join("adir")).unwrap();
        fs::write(temp.path().join("b.txt"), b"x").unwrap();
        let entries = read_entries(temp.path()).unwrap();
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["..", "adir", "zdir", "b.txt"]);
    }

    #[test]
    fn goes_up_from_a_nested_directory() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        let mut browser = Browser::new(child.clone(), "uidl".to_string(), 0, "a.bin".to_string());
        assert_eq!(browser.dir, child);
        assert_eq!(Browser::entry_label(&browser.entries[0]), "..（上一级）");
        assert!(browser.go_up());
        assert_eq!(browser.dir, temp.path());

        browser.selected = 0;
        browser.enter();
        assert_eq!(browser.dir, temp.path().parent().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn lists_other_drive_letters_at_the_root() {
        assert!(Path::new("D:\\").parent().is_none());
        let mask = (1u32 << 2) | (1u32 << 3);
        let entries = drives_from_mask(mask, Path::new("D:\\"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "C:\\");
        assert!(entries[0].is_dir);
        assert_eq!(Browser::entry_label(&entries[0]), "C:\\");
    }

    #[test]
    fn filters_by_name_and_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("alpha")).unwrap();
        fs::create_dir(temp.path().join("beta")).unwrap();
        fs::write(temp.path().join("notes.txt"), b"x").unwrap();
        let mut browser = Browser::new(
            temp.path().to_path_buf(),
            "uidl".to_string(),
            0,
            "a.bin".to_string(),
        );

        browser.filter = "be".to_string();
        browser.apply_filter();
        let names: Vec<_> = browser
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["..", "beta"]);
        assert_eq!(browser.filter_count(), 1);

        browser.filter = "notes".to_string();
        browser.apply_filter();
        assert!(browser
            .entries
            .iter()
            .any(|entry| entry.name == "notes.txt"));

        let target = temp.path().join("alpha");
        browser.filter = target.to_string_lossy().to_string();
        browser.apply_filter();
        let names: Vec<_> = browser
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["..", "alpha"]);
        assert_eq!(browser.filter_jump_target(), Some(target));
    }

    #[test]
    fn filters_chinese_names_by_pinyin_initials() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["项目 管理", "项目资料", "会议纪要", "归档"] {
            fs::create_dir(temp.path().join(name)).unwrap();
        }
        let mut browser = Browser::new(
            temp.path().to_path_buf(),
            "uidl".to_string(),
            0,
            "a.bin".to_string(),
        );
        let names = |browser: &Browser| -> Vec<String> {
            browser
                .entries
                .iter()
                .filter(|entry| entry.name != "..")
                .map(|entry| entry.name.clone())
                .collect()
        };

        browser.filter = "xm".to_string();
        browser.apply_filter();
        let matched = names(&browser);
        assert_eq!(matched.len(), 2, "{matched:?}");
        assert!(matched.iter().any(|name| name == "项目 管理"));
        assert!(matched.iter().any(|name| name == "项目资料"));

        browser.filter = "xmgl".to_string();
        browser.apply_filter();
        assert_eq!(names(&browser), vec!["项目 管理"]);

        browser.filter = "hyjy".to_string();
        browser.apply_filter();
        assert_eq!(names(&browser), vec!["会议纪要"]);

        browser.filter = "项目".to_string();
        browser.apply_filter();
        assert_eq!(names(&browser).len(), 2);
    }

    #[test]
    fn path_filter_accepts_pinyin_initials() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("项目")).unwrap();
        fs::create_dir(temp.path().join("归档")).unwrap();
        let mut browser = Browser::new(
            temp.path().to_path_buf(),
            "uidl".to_string(),
            0,
            "a.bin".to_string(),
        );

        let mut query = temp.path().to_string_lossy().to_string();
        query.push(std::path::MAIN_SEPARATOR);
        query.push_str("xm");
        browser.filter = query;
        browser.apply_filter();
        let names: Vec<_> = browser
            .entries
            .iter()
            .filter(|entry| entry.name != "..")
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["项目"]);
    }

    #[test]
    fn jumps_to_a_fuzzy_history_match_and_records_the_visit() {
        let temp = tempfile::tempdir().unwrap();
        let start = temp.path().join("start");
        let target = temp.path().join("Projects").join("zz-unique-target");
        fs::create_dir_all(&start).unwrap();
        fs::create_dir_all(&target).unwrap();
        let history_file = temp.path().join("dir_history.json");
        {
            let mut history = DirHistory::load(history_file.clone());
            history.record(&target);
        }

        let mut browser = Browser::new(start.clone(), "uidl".to_string(), 0, "a.bin".to_string());
        browser.attach_history(DirHistory::load(history_file.clone()));
        browser.open_jump();
        assert_eq!(browser.jump.as_ref().unwrap().matches, vec![target.clone()]);

        browser.jump.as_mut().unwrap().query = "zztarget".to_string();
        browser.update_jump();
        assert!(browser.jump.as_ref().unwrap().matches.contains(&target));
        assert_eq!(browser.jump_target(), Some(target.clone()));
        assert_eq!(browser.jump_to_selected(), Some(target.clone()));
        assert_eq!(browser.dir, target);
        assert!(browser.jump.is_none());

        let reloaded = DirHistory::load(history_file);
        let paths = reloaded.search("", 10, None);
        assert!(paths.contains(&start), "{paths:?}");
        assert!(paths.contains(&target), "{paths:?}");
    }

    #[test]
    fn filename_targets_stay_inside_directory() {
        let temp = tempfile::tempdir().unwrap();
        let mut browser = Browser::new(
            temp.path().to_path_buf(),
            "uidl".to_string(),
            0,
            "report.pdf".to_string(),
        );
        assert_eq!(
            browser.file_target().unwrap(),
            temp.path().join("report.pdf")
        );
        browser.filename = "../escape.pdf".to_string();
        assert!(browser.file_target().is_none());
        browser.filename = "..".to_string();
        assert!(browser.file_target().is_none());
    }

    #[test]
    fn creates_and_enters_subdirectory() {
        let temp = tempfile::tempdir().unwrap();
        let mut browser = Browser::new(
            temp.path().to_path_buf(),
            "uidl".to_string(),
            0,
            "a.bin".to_string(),
        );
        browser.create_dir("收件箱").unwrap();
        let index = browser
            .entries
            .iter()
            .position(|entry| entry.name == "收件箱")
            .unwrap();
        assert!(browser.entries[index].is_dir);
        browser.selected = index;
        browser.enter();
        assert_eq!(browser.dir, temp.path().join("收件箱"));
    }

    #[test]
    fn sanitizes_paths_and_illegal_characters() {
        assert_eq!(sanitize_filename("../../etc/passwd", 0), "passwd");
        assert_eq!(sanitize_filename("a:b?.pdf", 0), "a_b_.pdf");
        assert_eq!(sanitize_filename("..", 2), "attachment-3.bin");
        assert_eq!(sanitize_filename("", 0), "attachment-1.bin");
    }

    #[test]
    fn missing_start_falls_back_to_an_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("does-not-exist");
        let browser = Browser::new(missing, "uidl".to_string(), 0, "a.bin".to_string());
        assert!(browser.dir.is_dir());
        assert!(browser.note.is_some());
    }
}
