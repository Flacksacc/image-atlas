//! Human-readable presentation of Fab and Launcher pack metadata.
use eframe::egui;
use serde_json::Value;
use std::collections::BTreeSet;

fn label(key: &str) -> String {
    match key {
        "assetId" => return "Epic catalog asset ID".into(),
        "legacyItemId" => return "Marketplace ID".into(),
        "assetNamespace" => return "Catalog namespace".into(),
        "artifactId" => return "Download identifier".into(),
        "distributionMethod" => return "Pack type".into(),
        "url" => return "Link".into(),
        _ => {}
    }
    let chars: Vec<_> = key.chars().collect();
    let mut result = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' { result.push(' '); continue; }
        if i > 0 && c.is_uppercase() && (chars[i-1].is_lowercase()
            || (chars[i-1].is_uppercase() && chars.get(i+1).is_some_and(|c| c.is_lowercase()))) {
            result.push(' ');
        }
        result.push(if i == 0 { c.to_ascii_uppercase() } else { c });
    }
    result
}

fn text(key: &str, value: &Value) -> String {
    match value {
        Value::Bool(value) => if *value { "Yes" } else { "No" }.into(),
        Value::String(value) => {
            if let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) {
                return date.format("%b %-d, %Y · %H:%M %:z").to_string();
            }
            if value.starts_with("UE_") { return format!("Unreal Engine {}", &value[3..]); }
            if key == "distributionMethod" { return label(&value.to_ascii_lowercase()); }
            value.clone()
        }
        Value::Number(value) => {
            let lower = key.to_ascii_lowercase();
            if lower.contains("bytes") || lower == "filesize" || lower == "downloadsize" {
                if let Some(size) = value.as_f64() {
                    for (divisor, unit) in [(1_073_741_824., "GiB"), (1_048_576., "MiB"), (1024., "KiB")] {
                        if size >= divisor { return format!("{:.1} {unit}", size / divisor); }
                    }
                    return format!("{size} B");
                }
            }
            value.to_string()
        }
        _ => String::new(),
    }
}

fn populated(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => values.iter().any(populated),
        Value::Object(values) => values.values().any(populated),
        _ => true,
    }
}

fn field(ui: &mut egui::Ui, title: &str, value: &str) {
    ui.label(egui::RichText::new(title).strong());
    ui.label(value);
    ui.add_space(4.0);
}

fn render(ui: &mut egui::Ui, key: &str, value: &Value, depth: usize) {
    if !populated(value) { return; }
    ui.push_id(key, |ui| {
        match value {
            Value::Object(values) => {
                ui.collapsing(label(key), |ui| {
                    for (key, value) in values { render(ui, key, value, depth + 1); }
                });
            }
            Value::Array(values) if values.iter().all(|v| !v.is_object() && !v.is_array()) => {
                let values: Vec<_> = values.iter().filter(|v| populated(v)).map(|v| text(key, v)).collect();
                field(ui, &label(key), &values.join(", "));
            }
            Value::Array(values) => {
                ui.collapsing(format!("{} ({})", label(key), values.iter().filter(|v| populated(v)).count()), |ui| {
                    for (index, value) in values.iter().enumerate() {
                        if !populated(value) { continue; }
                        let title = ["name", "title", "type", "platform", "artifactId"]
                            .iter().find_map(|key| value.get(key).and_then(Value::as_str))
                            .map(str::to_owned).unwrap_or_else(|| format!("Item {}", index + 1));
                        ui.push_id(index, |ui| {
                            ui.collapsing(title, |ui| {
                                if let Some(values) = value.as_object() {
                                    for (key, value) in values { render(ui, key, value, depth + 1); }
                                } else { render(ui, "Value", value, depth + 1); }
                            });
                        });
                    }
                });
            }
            _ => {
                let value = text(key, value);
                if (value.starts_with("https://") || value.starts_with("http://")) && !value.contains(char::is_whitespace) {
                    ui.hyperlink_to(format!("Open {}", label(key).to_lowercase()), value);
                } else { field(ui, &label(key), &value); }
            }
        }
    });
}

pub fn show_pack_metadata(ui: &mut egui::Ui, raw: &str) {
    let Ok(Value::Object(values)) = serde_json::from_str::<Value>(raw) else {
        ui.label("Pack metadata is unavailable."); return;
    };
    let mut engines = BTreeSet::new();
    let mut platforms = BTreeSet::new();
    if let Some(versions) = values.get("projectVersions").and_then(Value::as_array) {
        for version in versions {
            for (key, target) in [("engineVersions", &mut engines), ("targetPlatforms", &mut platforms)] {
                if let Some(items) = version.get(key).and_then(Value::as_array) {
                    for item in items.iter().filter_map(Value::as_str) { target.insert(item.strip_prefix("UE_").unwrap_or(item).to_owned()); }
                }
            }
        }
    }
    ui.heading("Pack information");
    if let Some(categories) = values.get("categories").and_then(Value::as_array) {
        let names: Vec<_> = categories.iter().filter_map(|item| item.as_str().or_else(|| item.get("name").and_then(Value::as_str))).collect();
        if !names.is_empty() { field(ui, "Categories", &names.join(", ")); }
    }
    if let Some(value) = values.get("distributionMethod").filter(|v| populated(v)) {
        field(ui, "Pack type", &text("distributionMethod", value));
    }
    if !engines.is_empty() {
        let mut engines: Vec<_> = engines.into_iter().collect();
        engines.sort_by_key(|version| version.split('.').map(|part| part.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>());
        field(ui, "Supported Unreal versions", &engines.join(", "));
    }
    if !platforms.is_empty() { field(ui, "Target platforms", &platforms.into_iter().collect::<Vec<_>>().join(", ")); }
    let technical = ["assetId", "assetNamespace", "legacyItemId", "customAttributes", "AppName", "CatalogItemId", "CatalogNamespace"];
    for (key, value) in &values {
        if !technical.contains(&key.as_str()) && !["title", "description", "source", "url", "categories", "distributionMethod", "projectVersions"].contains(&key.as_str()) {
            render(ui, key, value, 0);
        }
    }
    if let Some(versions) = values.get("projectVersions") { render(ui, "Download versions", versions, 0); }
    ui.collapsing("Technical identifiers", |ui| {
        for key in technical { if let Some(value) = values.get(key) { render(ui, key, value, 0); } }
    });
}
