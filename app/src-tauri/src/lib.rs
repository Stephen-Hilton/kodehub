//! Kodehub desktop: a thin Tauri v2 shell. The UI talks to the local kodehub-server over
//! HTTP/WebSocket (http://127.0.0.1:7878 by default; override in the UI via localStorage
//! `kodehub.server`). Sign-in happens in the UI; this shell handles no credentials.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Kodehub desktop app");
}
