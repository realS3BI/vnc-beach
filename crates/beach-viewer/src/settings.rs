//! Connection settings: global defaults in `config.toml`, and per-favorite
//! overrides that replace single fields.

use rfb::PixelFormat;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    /// Scale the remote screen to the window.
    Fit,
    /// Show it pixel for pixel and scroll.
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colors {
    /// 24-bit true colour.
    Full,
    /// 16-bit (RGB565).
    Medium,
    /// 8-bit (256 colours), for slow links.
    Low,
}

impl Colors {
    pub fn pixel_format(self) -> PixelFormat {
        match self {
            Colors::Full => PixelFormat::BGRX,
            Colors::Medium => PixelFormat::RGB565,
            Colors::Low => PixelFormat::BGR233,
        }
    }
}

/// How Ctrl and Cmd are sent, for Mac ↔ Windows sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shortcuts {
    /// Swap when the other side is a different kind of machine.
    Auto,
    /// Always send Ctrl as Cmd and Cmd as Ctrl.
    Swap,
    /// Send keys as pressed.
    Plain,
}

/// Everything that can be set per connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Watch only, send no keyboard or mouse input.
    pub view_only: bool,
    pub scaling: Scaling,
    pub colors: Colors,
    /// Sync the clipboard in both directions.
    pub clipboard: bool,
    pub shortcuts: Shortcuts,
    /// Go fullscreen as soon as the connection is up.
    pub fullscreen: bool,
    /// Resize the window to the remote screen on connect.
    pub fit_window: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            view_only: false,
            scaling: Scaling::Fit,
            colors: Colors::Full,
            clipboard: true,
            shortcuts: Shortcuts::Auto,
            fullscreen: false,
            fit_window: true,
        }
    }
}

/// A favorite's deviations from the defaults. `None` means "use the default".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scaling: Option<Scaling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<Colors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcuts: Option<Shortcuts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_window: Option<bool>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }

    pub fn apply(&self, base: Settings) -> Settings {
        Settings {
            view_only: self.view_only.unwrap_or(base.view_only),
            scaling: self.scaling.unwrap_or(base.scaling),
            colors: self.colors.unwrap_or(base.colors),
            clipboard: self.clipboard.unwrap_or(base.clipboard),
            shortcuts: self.shortcuts.unwrap_or(base.shortcuts),
            fullscreen: self.fullscreen.unwrap_or(base.fullscreen),
            fit_window: self.fit_window.unwrap_or(base.fit_window),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_single_fields() {
        let base = Settings::default();
        let over = Overrides {
            colors: Some(Colors::Low),
            clipboard: Some(false),
            ..Default::default()
        };
        assert!(!over.is_empty());
        assert!(Overrides::default().is_empty());
        let s = over.apply(base);
        assert_eq!(
            (s.colors, s.clipboard, s.scaling, s.view_only),
            (Colors::Low, false, Scaling::Fit, false)
        );
    }

    #[test]
    fn toml_omits_unset_overrides() {
        let over = Overrides {
            scaling: Some(Scaling::Original),
            ..Default::default()
        };
        let text = toml::to_string(&over).unwrap();
        assert_eq!(text.trim(), "scaling = \"original\"");
        assert_eq!(toml::from_str::<Overrides>(&text).unwrap(), over);
        assert_eq!(toml::from_str::<Overrides>("").unwrap(), Overrides::default());
    }
}
