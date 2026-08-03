use familiar_core::FamiliarError;

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

    #[test]
    fn icon_has_expected_dimensions() {
        let icon = load_icon().unwrap();
        assert_eq!(icon.width, 32);
        assert_eq!(icon.height, 32);
    }
}
