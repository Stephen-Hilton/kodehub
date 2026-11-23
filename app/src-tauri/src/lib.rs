//! Kodehub desktop: a thin Tauri v2 shell. The UI talks to the local kodehub-server over
//! HTTP/WebSocket (http://127.0.0.1:7878 by default; override in the UI via localStorage
//! `kodehub.server`). No credentials are ever handled here.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![ui_token])
        .run(tauri::generate_context!())
        .expect("error while running the Kodehub desktop app");
}

/// The local server's per-launch UI secret ($KODEHUB_HOME/ui-token, default ~/.kode).
#[tauri::command]
fn ui_token() -> Result<String, String> {
    let home = std::env::var("KODEHUB_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".kode"));
    std::fs::read_to_string(home.join("ui-token"))
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("kodehub-server not running? {e}"))
}
