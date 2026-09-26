//! pet pack and ask/permission-answer commands.
//! Mechanical move from main.rs.

#[tauri::command]
pub(crate) fn list_pet_packs() -> Vec<crate::core::petpack::PetPackInfo> {
    crate::core::petpack::list()
}

/// Import a pet pack: directory, single image, or 3D model (.glb/.gltf).
#[tauri::command]
pub(crate) fn import_pet_pack(path: String) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if p.is_dir() {
        return crate::core::petpack::install_from_dir(&p);
    }
    if !p.is_file() {
        return Err(format!("{path} does not exist"));
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        crate::core::petpack::install_from_model(&p, None)
    } else {
        crate::core::petpack::install_from_image(&p, None)
    }
}

/// Read the 3D model's raw bytes. Uses `ipc::Response` to pass raw bytes, not JSON —
/// the model is binary, and serializing it into a numeric array would inflate it several times over.
#[tauri::command]
pub(crate) fn read_pet_model(id: String) -> Result<tauri::ipc::Response, String> {
    crate::core::petpack::model_bytes(&id).map(tauri::ipc::Response::new)
}

/// Read auxiliary assets referenced by `.gltf` (.bin / textures) so the frontend can assemble a self-contained model.
#[tauri::command]
pub(crate) fn read_pet_asset(id: String, rel: String) -> Result<tauri::ipc::Response, String> {
    crate::core::petpack::model_asset(&id, &rel).map(tauri::ipc::Response::new)
}

/// Download an image from a direct URL and install it as a pet pack (download on the Rust side: bypasses CORS + can validate content-type).
#[tauri::command]
pub(crate) fn import_pet_pack_from_url(
    url: String,
    name: Option<String>,
) -> Result<String, String> {
    crate::core::petpack::install_from_url(&url, name.as_deref())
}

#[tauri::command]
pub(crate) fn delete_pet_pack(id: String) -> Result<(), String> {
    crate::core::petpack::delete(&id)
}

/// Read the sprite sheet as a data URL for WebView rendering (avoids extra asset-protocol scope config).
#[tauri::command]
pub(crate) fn read_pet_sheet(id: String) -> Result<String, String> {
    crate::core::petpack::sheet_data_url(&id)
}

#[tauri::command]
pub(crate) fn answer_ask(id: String, answer: String) -> bool {
    crate::core::rpc::resolve_ask(&id, &answer)
}

#[tauri::command]
pub(crate) fn answer_permission(id: String, answer: String) -> bool {
    crate::core::permission::resolve_ask(&id, &answer)
}

#[tauri::command]
pub(crate) fn answer_install(id: String, answer: String) -> bool {
    crate::core::permission::resolve_ask(&id, &answer)
}

#[tauri::command]
pub(crate) fn list_plugins() -> Vec<crate::core::plugin::PluginStatusDto> {
    crate::core::plugin::PluginManager::shared().list()
}
