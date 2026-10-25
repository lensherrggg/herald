use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Debug)]
pub struct Browser {
    pub dir: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub selected: usize,
    pub filename: String,
    pub filename_focus: bool,
    pub uidl: String,
    pub attachment_index: usize,
    pub original_name: String,
    pub note: Option<String>,
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
            entries: Vec::new(),
            selected: 0,
            filename: original_name.clone(),
            filename_focus: false,
            uidl,
            attachment_index,
            original_name,
            note,
        };
        browser.refresh();
        browser
    }

    pub fn refresh(&mut self) {
        match read_entries(&self.dir) {
            Ok(entries) => {
                self.entries = entries;
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
            }
            Err(err) => {
                let fallback = dirs_home().unwrap_or_else(|| PathBuf::from("."));
                if fallback != self.dir {
                    self.note = Some(format!(
                        "无法读取 {}（{err}），已转到 {}",
                        self.dir.display(),
                        fallback.display()
                    ));
                    self.dir = fallback;
                    self.entries = read_entries(&self.dir).unwrap_or_default();
                    self.selected = 0;
                } else {
                    self.entries.clear();
                    self.selected = 0;
                    self.note = Some(format!("无法读取目录：{err}"));
                }
            }
        }
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
            self.dir = entry.path;
            self.selected = 0;
            self.refresh();
        } else {
            self.filename = entry.name;
            self.filename_focus = true;
        }
    }

    pub fn focus_filename(&mut self) {
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
        self.refresh();
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            self.selected = index;
        }
        Ok(())
    }

    pub fn entry_label(entry: &BrowserEntry) -> String {
        if entry.is_dir {
            if entry.name == ".." {
                "..".to_string()
            } else {
                format!("{}/", entry.name)
            }
        } else {
            entry.name.clone()
        }
    }
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
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);
    Ok(dirs)
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
