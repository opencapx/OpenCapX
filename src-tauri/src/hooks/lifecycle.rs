//! install/uninstall of hook config per host.
//! Mechanical move from hooks.rs.

use super::*;

pub(crate) fn install(kind: &str) -> std::io::Result<()> {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "unknown agent",
        ));
    };
    let cmd = full_command(kind);

    if s.style == Style::OpencodePluginModule {
        std::fs::create_dir_all(&path)?;
        std::fs::write(path.join("index.js"), opencode_plugin(&binary_from(&cmd)))?;
        std::fs::write(path.join("package.json"), opencode_plugin_package_json())?;
        register_opencode_plugin(&path)?;
        return Ok(());
    }
    if s.style == Style::PiExtension || s.style == Style::OmpExtension {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = if s.style == Style::OmpExtension {
            omp_extension(&binary_from(&cmd))
        } else {
            pi_extension(&binary_from(&cmd))
        };
        return std::fs::write(&path, body);
    }

    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not a JSON object; fix or remove it and try again",
                path.display()
            ),
        ));
    };
    if s.style == Style::CursorFlat {
        obj.entry("version").or_insert(json!(1));
    }
    if s.style == Style::KiroFlat && obj.get("name").is_none() {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "default".into());
        obj.insert("name".to_string(), json!(name));
    }
    let key = container_key(s.style);
    if !obj.get(key).map_or(false, |h| h.is_object()) {
        obj.insert(key.to_string(), json!({}));
    }
    let map = obj.get_mut(key).and_then(|h| h.as_object_mut()).unwrap();
    for event in s.events {
        let mut kept: Vec<Value> = map
            .get(*event)
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter(|e| !entry_is_ours(s.style, event, e))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        kept.push(make_entry(s.style, event, &cmd));
        map.insert((*event).to_string(), Value::Array(kept));
    }
    write_json(&path, &v)?;
    if kind == "codex" {
        enable_codex_hooks();
    }
    Ok(())
}

pub(crate) fn uninstall(kind: &str) -> std::io::Result<()> {
    let (Some(path), Some(s)) = (config_path(kind), spec(kind)) else {
        return Ok(());
    };
    if s.style == Style::OpencodePluginModule {
        unregister_opencode_plugin(&path);
        let _ = std::fs::remove_dir_all(&path);
        return Ok(());
    }
    if kind == "copilot" || s.style == Style::PiExtension || s.style == Style::OmpExtension {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let mut v = read_json(&path);
    let Some(obj) = v.as_object_mut() else {
        return Ok(());
    };
    let key = container_key(s.style);
    if let Some(map) = obj.get_mut(key).and_then(|h| h.as_object_mut()) {
        for event in s.events {
            if let Some(arr) = map.get(*event).and_then(|a| a.as_array()) {
                let kept: Vec<Value> = arr
                    .iter()
                    .filter(|e| !entry_is_ours(s.style, event, e))
                    .cloned()
                    .collect();
                if kept.is_empty() {
                    map.remove(*event);
                } else {
                    map.insert((*event).to_string(), Value::Array(kept));
                }
            }
        }
        if map.is_empty() {
            obj.remove(key);
        }
    }
    write_json(&path, &v)
}

pub(crate) fn binary_from(cmd: &str) -> String {
    if let Some(start) = cmd.find('"') {
        if let Some(end) = cmd[start + 1..].find('"') {
            return cmd[start + 1..start + 1 + end].to_string();
        }
    }
    cmd.split(' ').next().unwrap_or(cmd).to_string()
}
