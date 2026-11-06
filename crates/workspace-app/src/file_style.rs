//! Static Nerd Font file identities. Never executes a personal icon provider.
use crate::appearance::Palette;
use std::path::Path;

pub struct FileStyle {
    pub glyph: &'static str,
    pub color: u32,
}

pub fn style(path: &Path, directory: bool, expanded: bool, p: Palette) -> FileStyle {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let name = name.to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (glyph, color) = if directory {
        (if expanded { "" } else { "" }, p.green)
    } else if matches!(
        name.as_str(),
        "dockerfile" | "compose.yaml" | "compose.yml" | "docker-compose.yml"
    ) {
        ("", p.accent)
    } else if name.starts_with(".git") {
        ("", p.orange)
    } else if matches!(name.as_str(), "cargo.toml" | "cargo.lock") {
        ("", p.orange)
    } else {
        match ext.as_str() {
            "rs" => ("", p.orange),
            "py" | "pyi" => ("", p.orange),
            "tf" | "tfvars" | "tfstate" | "hcl" => ("", p.accent),
            "js" | "jsx" | "mjs" | "cjs" => ("", p.orange),
            "ts" | "tsx" => ("", p.accent),
            "lua" => ("", p.accent),
            "go" => ("", p.green),
            "c" | "h" | "cpp" | "hpp" | "swift" => ("", p.accent),
            "html" | "htm" => ("", p.orange),
            "css" | "scss" => ("", p.accent),
            "json" | "jsonc" => ("", p.orange),
            "yaml" | "yml" | "toml" | "ini" | "conf" => ("", p.muted),
            "md" | "mdx" => ("", p.accent),
            "csv" | "tsv" | "xlsx" | "xls" => ("󰈛", p.green),
            "sh" | "zsh" | "fish" | "bash" => ("", p.green),
            "png" | "jpg" | "jpeg" | "svg" | "webp" | "gif" => ("", p.accent),
            "mov" | "mp4" | "webm" => ("", p.accent),
            "pdf" => ("", p.orange),
            "zip" | "gz" | "tar" => ("", p.orange),
            _ => ("", p.muted),
        }
    };
    FileStyle { glyph, color }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Theme;
    #[test]
    fn filenames_override_extensions_and_directories_override_names() {
        let p = Theme::FirefoxRose.palette(false);
        assert_eq!(style(Path::new("Cargo.toml"), false, false, p).glyph, "");
        assert_eq!(style(Path::new("view.TSX"), false, false, p).glyph, "");
        for name in [
            "main.tf",
            "vars.tfvars",
            ".terraform.lock.hcl",
            "state.tfstate",
        ] {
            let terraform = style(Path::new(name), false, false, p);
            assert_eq!(terraform.glyph, "");
            assert_eq!(terraform.color, p.accent);
        }
        assert_eq!(style(Path::new("Dockerfile"), false, false, p).glyph, "");
        assert_eq!(style(Path::new(".gitignore"), false, false, p).glyph, "");
        assert_eq!(style(Path::new("code.rs"), true, true, p).glyph, "");
        assert_eq!(
            style(Path::new("table.csv"), false, false, p).color,
            p.green
        );
        assert_eq!(style(Path::new("unknown"), false, false, p).color, p.muted);
    }
}
