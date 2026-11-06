//! Built-in app palettes, never evaluates or reads user theme configuration.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    CatppuccinMocha,
    NightOwl,
    FirefoxRose,
    SolarizedDark,
    SelenizedDark,
}
impl Theme {
    pub const ALL: [Self; 8] = [
        Self::System,
        Self::Light,
        Self::Dark,
        Self::CatppuccinMocha,
        Self::NightOwl,
        Self::FirefoxRose,
        Self::SolarizedDark,
        Self::SelenizedDark,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::NightOwl => "night-owl",
            Self::FirefoxRose => "firefox-rose",
            Self::SolarizedDark => "solarized-dark",
            Self::SelenizedDark => "selenized-dark",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == value)
    }
    pub fn palette(self, system_light: bool) -> Palette {
        match self {
            Self::System => {
                if system_light {
                    Self::Light.palette(false)
                } else {
                    Self::Dark.palette(false)
                }
            }
            Self::Light => Palette {
                bg: 0xf7f7f9,
                chrome: 0xeeeff3,
                panel: 0xf1f2f6,
                term: 0xfbfcfe,
                text: 0x292e3c,
                muted: 0x626b7e,
                line: 0xdce0e9,
                active: 0xe3dff4,
                accent: 0x69539a,
                green: 0x24705b,
                orange: 0x8c5c19,
            },
            Self::Dark => Palette {
                bg: 0x17191f,
                chrome: 0x20222a,
                panel: 0x1d1f27,
                term: 0x14161c,
                text: 0xdce0ec,
                muted: 0x9ca5bb,
                line: 0x303440,
                active: 0x353147,
                accent: 0xb8a4e5,
                green: 0x89c7af,
                orange: 0xe1b773,
            },
            // Explicit colors from static nvim profiles. Native surfaces are an adaptation.
            Self::CatppuccinMocha => Palette {
                bg: 0x1e1e2e,
                chrome: 0x1e1e2e,
                panel: 0x1e1e2e,
                term: 0x1e1e2e,
                text: 0xffffff,
                muted: 0xcdd6f4,
                line: 0x6c7086,
                active: 0x45475a,
                accent: 0xcba6f7,
                green: 0xa6e3a1,
                orange: 0xf9e2af,
            },
            Self::FirefoxRose => Palette {
                bg: 0x021c23,
                chrome: 0x02404e,
                panel: 0x023440,
                term: 0x021c23,
                text: 0xffffff,
                muted: 0xa7c0cc,
                line: 0x7395a3,
                active: 0x285565,
                accent: 0xbb9bff,
                green: 0x56e6bd,
                orange: 0xffca70,
            },
            // Native adaptation of the static profile/lualine palettes. Muted text
            // uses the readable foreground rather than the inactive-mode dim color.
            Self::SolarizedDark => Palette {
                bg: 0x002b36,
                chrome: 0x073642,
                panel: 0x073642,
                term: 0x002b36,
                text: 0xeee8d5,
                muted: 0x93a1a1,
                line: 0x586e75,
                active: 0x073642,
                accent: 0x6c71c4,
                green: 0x859900,
                orange: 0xb58900,
            },
            Self::SelenizedDark => Palette {
                bg: 0x103c48,
                chrome: 0x184956,
                panel: 0x184956,
                term: 0x103c48,
                text: 0xcad8d9,
                muted: 0xadbcbd,
                line: 0x72898f,
                active: 0x184956,
                accent: 0xaf88eb,
                green: 0x75b938,
                orange: 0xdbb32d,
            },
            Self::NightOwl => Palette {
                bg: 0x011627,
                chrome: 0x0b1e2f,
                panel: 0x0b1e2f,
                term: 0x011627,
                text: 0xd6deeb,
                muted: 0x8a9fac,
                line: 0x3e5c73,
                active: 0x1a2c3d,
                accent: 0x82aaff,
                green: 0xc3e88d,
                orange: 0xffcb6b,
            },
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FontFamily {
    #[default]
    Menlo,
    JetbrainsMono,
}
impl FontFamily {
    pub fn name(self) -> &'static str {
        match self {
            Self::Menlo => "Menlo",
            Self::JetbrainsMono => "JetBrainsMono Nerd Font",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Menlo => "menlo",
            Self::JetbrainsMono => "jetbrains-mono",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTypography")]
pub struct Typography {
    pub family: FontFamily,
    size: u8,
}
#[derive(Deserialize)]
struct RawTypography {
    family: FontFamily,
    size: u8,
}
impl TryFrom<RawTypography> for Typography {
    type Error = String;
    fn try_from(raw: RawTypography) -> Result<Self, String> {
        if !(9..=24).contains(&raw.size) {
            return Err("Tamaño de fuente fuera de9..24".into());
        }
        Ok(Self {
            family: raw.family,
            size: raw.size,
        })
    }
}
impl Default for Typography {
    fn default() -> Self {
        Self {
            family: FontFamily::Menlo,
            size: 13,
        }
    }
}
impl Typography {
    pub fn size(self) -> u8 {
        self.size
    }
    pub fn pixels(self) -> f32 {
        self.size as f32
    }
    pub fn line_height(self) -> f32 {
        (self.pixels() * 21. / 13.).round()
    }
    pub fn apply(self, command: &str) -> Result<Self, String> {
        let mut next = self;
        match command {
            "menlo" => next.family = FontFamily::Menlo,
            "jetbrains-mono" => next.family = FontFamily::JetbrainsMono,
            value => {
                let size = match value {
                    "+" => self.size.checked_add(1),
                    "-" => self.size.checked_sub(1),
                    _ => value.parse::<u8>().ok(),
                }
                .ok_or("Fuente o tamaño desconocido")?;
                if !(9..=24).contains(&size) {
                    return Err("Tamaño de fuente permitido:9..24".into());
                }
                next.size = size;
            }
        }
        Ok(next)
    }
}
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: u32,
    pub chrome: u32,
    pub panel: u32,
    pub term: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub active: u32,
    pub accent: u32,
    pub green: u32,
    pub orange: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typography_rejects_invalid_state_and_changes_metrics_without_changing_family() {
        let initial = Typography::default();
        let larger = initial.apply("+").unwrap().apply("jetbrains-mono").unwrap();
        assert_eq!(larger.size(), 14);
        assert!(larger.line_height() > initial.line_height());
        assert_eq!(larger.apply("-").unwrap().family, FontFamily::JetbrainsMono);
        for invalid in ["0", "8", "25", "NaN", "1e100", "/tmp/font.ttf"] {
            assert!(initial.apply(invalid).is_err());
        }
        for size in [0, 8, 25, 255] {
            assert!(
                serde_json::from_value::<Typography>(
                    serde_json::json!({"family":"menlo","size":size})
                )
                .is_err()
            );
        }
        assert_eq!(
            serde_json::from_str::<Typography>(&serde_json::to_string(&larger).unwrap()).unwrap(),
            larger
        );
    }
    fn luminance(rgb: u32) -> f64 {
        let channel = |s: u32| {
            let c = ((rgb >> s) & 255u32) as f64 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }
    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn text_and_picker_rows_remain_readable_in_all_palettes() {
        let mut ids = std::collections::BTreeSet::new();
        for theme in Theme::ALL {
            assert!(ids.insert(theme.id()));
            assert_eq!(Theme::parse(theme.id()), Some(theme));
            assert_eq!(
                serde_json::from_str::<Theme>(&serde_json::to_string(&theme).unwrap()).unwrap(),
                theme
            );
            for light in [true, false] {
                let p = theme.palette(light);
                for bg in [p.bg, p.panel, p.term, p.active] {
                    assert!(contrast(p.text, bg) >= 4.5, "{} text", theme.id());
                    assert!(contrast(p.muted, bg) >= 3., "{} muted", theme.id());
                }
            }
        }
        assert!(Theme::parse("../../theme.lua").is_none());
    }
}
