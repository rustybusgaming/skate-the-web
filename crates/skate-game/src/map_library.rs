//! Installed map discovery and the default pointer, shared by startup and the menu.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub label: String,
    pub path: Option<PathBuf>,
}

/// Maps left out of the menus even when installed: MegaPark is the
/// Super-Ultra Mega-Park cut out on its own, which spawns outside its geometry;
/// the full park is part of University.
fn hidden(path: &Path) -> bool {
    path.file_stem().is_some_and(|stem| stem.eq_ignore_ascii_case("MegaPark"))
}

/// How the game names its worlds; other maps (mods) keep their file names.
fn world_name(stem: &str) -> String {
    match stem.to_ascii_lowercase().as_str() {
        "university" => "University",
        "downtown" => "Downtown",
        "industrial" => "Industrial",
        "skateschool" => "Skate School",
        "maloofmoneycup" => "Maloof Money Cup",
        "blackboxpark" => "Black Box Park",
        "downtownskatepark" => "Downtown Skate Park",
        "industrialskatepark" => "Industrial Skate Park",
        "startpark" => "Start Park",
        "hawaiiandream" => "Hawaiian Dream",
        _ => return stem.replace('_', " "),
    }
    .into()
}

pub(crate) fn discover(assets: &Path) -> Result<Vec<Entry>, String> {
    let mut maps = Vec::new();
    let root = assets.parent().unwrap_or(assets).join("maps");
    let private = root.join("private");
    let mut directories = vec![
        skate_data::vfs::read_dir(&root)
            .map_err(|e| format!("{}: {e}", root.display()))?,
    ];
    match skate_data::vfs::read_dir(&private) {
        Ok(entries) => directories.push(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", private.display())),
    }
    for entries in directories {
        for entry in entries {
            let path = entry.path();
            if skate_data::vfs::is_file(&path)
                && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("skate"))
                && !hidden(&path)
            {
                maps.push(Entry {
                    label: world_name(&path.file_stem().unwrap().to_string_lossy()),
                    path: Some(path.canonicalize().unwrap_or(path)),
                });
            }
        }
    }
    maps.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    maps.insert(0, Entry { label: "Test world".into(), path: None });
    Ok(maps)
}

pub(crate) fn default_map(assets: &Path) -> Result<Option<PathBuf>, String> {
    // Only a completed release installation creates this pointer. Existing
    // development checkouts continue to boot the test world.
    let pointer = assets.parent().unwrap_or(assets).join("settings/default-map.json");
    let bytes = match skate_data::vfs::read(&pointer) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", pointer.display())),
    };
    let relative: Option<String> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let Some(relative) = relative else { return Ok(None); };
    let relative = Path::new(&relative);
    if relative.is_absolute() || relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        return Err("Invalid installed default map path".into());
    }
    Ok(Some(assets.parent().unwrap_or(assets).join(relative)))
}

/// Save only a successfully activated map. Null explicitly selects the test world.
pub(crate) fn save_default(assets: &Path, map: Option<&Path>) -> Result<(), String> {
    let root = assets.parent().unwrap_or(assets);
    let relative = map.map(|path| path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .map_err(|_| "Map is outside this installation; default map unchanged".to_string())).transpose()?;
    let directory = root.join("settings");
    skate_data::vfs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let temporary = directory.join("default-map.json.tmp");
    skate_data::vfs::write(&temporary, serde_json::to_vec_pretty(&relative).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    skate_data::vfs::rename(temporary, directory.join("default-map.json")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hawaiian_dream_map_has_a_friendly_menu_name() {
        assert_eq!(world_name("HawaiianDream"), "Hawaiian Dream");
    }

    #[test]
    fn discovery_reports_a_missing_maps_directory() {
        let assets = std::env::temp_dir()
            .join(format!("skate-map-discovery-{}", std::process::id()))
            .join("assets");
        assert!(discover(&assets).is_err());
    }

    #[test]
    fn default_pointer_round_trips_map_and_explicit_test_world() {
        let root = std::env::temp_dir().join(format!("skate-map-pointer-{}", std::process::id()));
        let assets = root.join("assets");
        let map = root.join("maps/example.skate");
        save_default(&assets, Some(&map)).unwrap();
        assert_eq!(default_map(&assets).unwrap(), Some(map));
        save_default(&assets, None).unwrap();
        assert_eq!(default_map(&assets).unwrap(), None);
        skate_data::vfs::write(root.join("settings/default-map.json"), br#""../outside.skate""#).unwrap();
        assert!(default_map(&assets).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
