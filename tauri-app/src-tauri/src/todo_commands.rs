use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

// ── Data model ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub id: u64,
    pub title: String,
    pub date: String,      // "YYYY-MM-DD"
    pub completed: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct TodoFile {
    items: Vec<TodoItem>,
}

// ── TodoStore ─────────────────────────────────────────────────────────────────

pub struct TodoStore {
    items: Vec<TodoItem>,
    path: PathBuf,
}

impl TodoStore {
    pub fn new() -> Self {
        let path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rust-air")
            .join("todos.json");
        let mut store = Self {
            items: Vec::new(),
            path,
        };
        store.load();
        store
    }

    /// For testing: create a TodoStore backed by a specific file path.
    #[cfg(test)]
    pub fn with_path(path: PathBuf) -> Self {
        let mut store = Self {
            items: Vec::new(),
            path,
        };
        store.load();
        store
    }

    fn load(&mut self) {
        self.items = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| {
                let s = String::from_utf8(bytes).ok()?;
                let file: TodoFile = serde_json::from_str(&s).ok()?;
                Some(file.items)
            })
            .unwrap_or_default();
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = TodoFile {
            items: self.items.clone(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn gen_id() -> u64 {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        ts + rand::random::<u16>() as u64
    }

    /// Get all items for a given date, sorted: uncompleted first, then completed.
    pub fn get_todos(&self, date: &str) -> Vec<TodoItem> {
        let mut result: Vec<TodoItem> = self
            .items
            .iter()
            .filter(|item| item.date == date)
            .cloned()
            .collect();
        result.sort_by_key(|item| item.completed);
        result
    }

    /// Add a new todo. Returns the created item.
    pub fn add_todo(&mut self, title: String, date: String) -> Result<TodoItem, String> {
        if title.trim().is_empty() {
            return Err("标题不能为空".to_string());
        }
        let item = TodoItem {
            id: Self::gen_id(),
            title,
            date,
            completed: false,
        };
        // Insert at the beginning so it appears at the top of the list.
        self.items.insert(0, item.clone());
        self.save()?;
        Ok(item)
    }

    /// Toggle the completed status of a todo. Returns the updated item.
    pub fn toggle_todo(&mut self, id: u64) -> Result<TodoItem, String> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "待办事项不存在".to_string())?;
        item.completed = !item.completed;
        let updated = item.clone();
        self.save()?;
        Ok(updated)
    }

    /// Delete a todo by id.
    pub fn delete_todo(&mut self, id: u64) -> Result<(), String> {
        let len_before = self.items.len();
        self.items.retain(|item| item.id != id);
        if self.items.len() == len_before {
            return Err("待办事项不存在".to_string());
        }
        self.save()?;
        Ok(())
    }

    /// Return dates in the given year/month that have at least one uncompleted todo.
    pub fn get_todo_dates(&self, year: i32, month: u32) -> Vec<String> {
        let prefix = format!("{:04}-{:02}-", year, month);
        let mut dates: Vec<String> = self
            .items
            .iter()
            .filter(|item| !item.completed && item.date.starts_with(&prefix))
            .map(|item| item.date.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        dates.sort();
        dates
    }
}

// ── Tauri IPC commands ────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_todos(date: String, store: State<'_, Mutex<TodoStore>>) -> Result<Vec<TodoItem>, String> {
    let store = store.lock().map_err(|e| e.to_string())?;
    Ok(store.get_todos(&date))
}

#[tauri::command]
pub fn add_todo(
    title: String,
    date: String,
    store: State<'_, Mutex<TodoStore>>,
) -> Result<TodoItem, String> {
    let mut store = store.lock().map_err(|e| e.to_string())?;
    store.add_todo(title, date)
}

#[tauri::command]
pub fn toggle_todo(id: u64, store: State<'_, Mutex<TodoStore>>) -> Result<TodoItem, String> {
    let mut store = store.lock().map_err(|e| e.to_string())?;
    store.toggle_todo(id)
}

#[tauri::command]
pub fn delete_todo(id: u64, store: State<'_, Mutex<TodoStore>>) -> Result<(), String> {
    let mut store = store.lock().map_err(|e| e.to_string())?;
    store.delete_todo(id)
}

#[tauri::command]
pub fn get_todo_dates(
    year: i32,
    month: u32,
    store: State<'_, Mutex<TodoStore>>,
) -> Result<Vec<String>, String> {
    let store = store.lock().map_err(|e| e.to_string())?;
    Ok(store.get_todo_dates(year, month))
}
