use std::path::PathBuf;

use chrono::{Local, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: u64,
    pub timestamp_ms: i64,
    pub app: String,
    pub duration_seconds: f32,
    pub text: String,
    pub engine: String,
    pub model: String,
}

#[derive(Deserialize, Serialize)]
struct DailyEntries {
    day: String,
    entries: Vec<Entry>,
}

pub struct History {
    path: PathBuf,
    stored: DailyEntries,
    next_id: u64,
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

impl History {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        Self::open_for_day(path, today())
    }

    fn open_for_day(path: PathBuf, day: String) -> Result<Self, String> {
        let stored: DailyEntries = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| "Invalid history file".to_owned())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DailyEntries {
                day: day.clone(),
                entries: Vec::new(),
            },
            Err(error) => return Err(error.to_string()),
        };
        let next_id = stored
            .entries
            .iter()
            .map(|entry| entry.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("History ID exhausted")?;
        let mut history = Self {
            path,
            stored,
            next_id,
        };
        history.refresh_for_day(day)?;
        Ok(history)
    }

    fn refresh_for_day(&mut self, day: String) -> Result<bool, String> {
        if self.stored.day != day {
            self.stored = DailyEntries {
                day,
                entries: Vec::new(),
            };
            self.next_id = 1;
            self.save()?;
            return Ok(true);
        }

        Ok(false)
    }

    pub fn expire(&mut self) -> Result<bool, String> {
        self.refresh_for_day(today())
    }

    fn save(&self) -> Result<(), String> {
        let parent = self.path.parent().ok_or("Invalid history path")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec(&self.stored).map_err(|error| error.to_string())?;
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }

    pub fn entries(&mut self) -> Result<Vec<Entry>, String> {
        self.refresh_for_day(today())?;
        Ok(self.stored.entries.clone())
    }

    pub fn add(
        &mut self,
        app: String,
        duration_seconds: f32,
        text: String,
        engine: String,
        model: String,
    ) -> Result<(), String> {
        self.refresh_for_day(today())?;
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or("History ID exhausted")?;
        self.stored.entries.push(Entry {
            id,
            timestamp_ms: Utc::now().timestamp_millis(),
            app,
            duration_seconds,
            text,
            engine,
            model,
        });
        self.save()
    }

    pub fn remove(&mut self, id: u64) -> Result<(), String> {
        self.refresh_for_day(today())?;
        self.stored.entries.retain(|entry| entry.id != id);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_only_the_current_calendar_day() {
        let path = std::env::temp_dir().join(format!(
            "yap-history-test-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let day = today();
        let mut history = History::open_for_day(path.clone(), day.clone()).unwrap();
        history
            .add(
                "Editor".into(),
                2.0,
                "private text".into(),
                "Local".into(),
                "base".into(),
            )
            .unwrap();
        let id = history.entries().unwrap()[0].id;
        let mut restored = History::open_for_day(path.clone(), day).unwrap();
        assert_eq!(restored.entries().unwrap()[0].text, "private text");
        restored.remove(id).unwrap();
        assert!(restored.entries().unwrap().is_empty());
        restored
            .add(
                "Editor".into(),
                1.0,
                "yesterday".into(),
                "Local".into(),
                "base".into(),
            )
            .unwrap();
        let next_day = (Local::now().date_naive() + chrono::Days::new(1)).to_string();
        assert!(restored.refresh_for_day(next_day.clone()).unwrap());
        assert!(!restored.refresh_for_day(next_day.clone()).unwrap());
        let next = History::open_for_day(path.clone(), next_day).unwrap();
        assert!(next.stored.entries.is_empty());
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("yesterday")
        );
        std::fs::remove_file(path).unwrap();
    }
}
