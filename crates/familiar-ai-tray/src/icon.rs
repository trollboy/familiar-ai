use familiar_ai_core::FamiliarError;

const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.png");

pub struct LoadedIcon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn load_icon() -> Result<LoadedIcon, FamiliarError> {
    let img = image::load_from_memory(ICON_BYTES)
        .map_err(|e| FamiliarError::Config(format!("failed to decode tray icon: {e}")))?
        .into_rgba8();
    let (width, height) = img.dimensions();
    Ok(LoadedIcon {
        rgba: img.into_raw(),
        width,
        height,
    })
}

pub fn load_tray_icon() -> Result<tray_icon::Icon, FamiliarError> {
    let loaded = load_icon()?;
    tray_icon::Icon::from_rgba(loaded.rgba, loaded.width, loaded.height)
        .map_err(|e| FamiliarError::Config(format!("failed to build tray icon: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_icon_successfully() {
        let icon = load_icon().unwrap();
        assert!(icon.width > 0);
        assert!(icon.height > 0);
        assert_eq!(icon.rgba.len() as u32, icon.width * icon.height * 4);
    }

    /// Pins the shipped asset so an accidental swap is caught. The pin read
    /// 32x32 until it was checked against the file for the first time in a
    /// long while: the asset became 512x512 in ddd3d7a and nothing noticed,
    /// because this crate sits outside the workspace and its tests were never
    /// part of any run. The tray host scales the icon, so 512 is fine — the
    /// stale number was the defect, not the artwork.
    #[test]
    fn icon_has_expected_dimensions() {
        let icon = load_icon().unwrap();
        assert_eq!(icon.width, 512);
        assert_eq!(icon.height, 512);
    }
}
