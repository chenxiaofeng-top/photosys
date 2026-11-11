mod database;

use std::fs;
use std::sync::Mutex;

use database::Database;
use tauri::{Manager, State};

#[tauri::command]
fn load_note(database: State<'_, Mutex<Database>>) -> Result<String, String> {
    database
        .lock()
        .map_err(|error| error.to_string())?
        .load_note()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn save_note(database: State<'_, Mutex<Database>>, text: String) -> Result<(), String> {
    database
        .lock()
        .map_err(|error| error.to_string())?
        .save_note(&text)
        .map_err(|error| error.to_string())
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let directory = app.path().app_local_data_dir()?;
            fs::create_dir_all(&directory)?;
            let database = Database::open(&directory.join("photosys.sqlite3"))?;
            app.manage(Mutex::new(database));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![load_note, save_note])
        .run(tauri::generate_context!())
        .expect("could not start PhotoSys");
}
