//! Recognition history: a JSON index plus the source images in `<data>/history/`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    /// RFC 3339 local time
    pub time: String,
    pub latex: String,
    /// What was pasted/copied (after the format template).
    pub output: String,
    pub format: String,
    /// Executable of the app it was pasted into, if any.
    pub app: Option<String>,
    pub app_name: Option<String>,
    pub image: Option<String>,
    pub ms: u64,
    #[serde(default)]
    pub favorite: bool,
}

pub struct History {
    dir: PathBuf,
    pub entries: Vec<Entry>, // newest first
}

impl History {
    pub fn load(data_dir: &std::path::Path) -> Self {
        let dir = data_dir.join("history");
        let _ = std::fs::create_dir_all(&dir);
        let entries = std::fs::read_to_string(data_dir.join("history.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { dir, entries }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.parent().unwrap().join("history.json")
    }

    pub fn save(&self) -> Result<()> {
        let tmp = self.index_path().with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.entries)?)?;
        std::fs::rename(tmp, self.index_path())?;
        Ok(())
    }

    pub fn new_id() -> String {
        chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string()
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.png"))
    }

    pub fn add(&mut self, entry: Entry, limit: usize) {
        self.entries.insert(0, entry);
        // Trim the oldest non-favorites beyond the limit.
        while self.entries.len() > limit {
            match self.entries.iter().rposition(|e| !e.favorite) {
                Some(i) => {
                    let e = self.entries.remove(i);
                    self.remove_image(&e);
                }
                None => break,
            }
        }
        self.persist();
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    pub fn delete(&mut self, id: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.id == id) {
            let e = self.entries.remove(i);
            self.remove_image(&e);
            self.persist();
        }
    }

    pub fn clear(&mut self, keep_favorites: bool) {
        let (keep, drop): (Vec<_>, Vec<_>) = self.entries.drain(..).partition(|e| keep_favorites && e.favorite);
        for e in &drop {
            self.remove_image(e);
        }
        self.entries = keep;
        self.persist();
    }

    fn remove_image(&self, e: &Entry) {
        if let Some(img) = &e.image
            && PathBuf::from(img).starts_with(&self.dir)
        {
            let _ = std::fs::remove_file(img);
        }
    }

    pub fn persist(&self) {
        if let Err(e) = self.save() {
            log::error!("saving history: {e:#}");
        }
    }
}
