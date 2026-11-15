//! Resolve the default LazyVim root sources without changing process cwd.
use std::{fs, io::ErrorKind, path::{Component, Path, PathBuf}};

fn validation_directory(cwd: &Path, root: &Path) -> Result<PathBuf, String> {
    use crate::tree_edit::{descend, parts, root_fd};
    let relative = cwd.strip_prefix(root).map_err(|_| "Ruta fuera del perfil de validación")?;
    let root_parts = parts(root.strip_prefix("/").map_err(|_| "Raíz de validación inválida")?)?;
    let base = descend(root_fd(Path::new("/"))?, &root_parts, false)?;
    let mut directories = vec![base];
    let mut names = Vec::new();
    for part in relative.components() {
        match part {
            Component::Normal(name) => {
                let next = parts(Path::new(name))?;
                if next.len() != 1 || names.len() == 64 { return Err("Directorio fuera de límites".into()); }
                let parent = directories.last().ok_or("Directorio no disponible")?
                    .try_clone().map_err(|e| e.to_string())?;
                directories.push(descend(parent, &next, false)?);
                names.push(name.to_os_string());
            }
            Component::CurDir => {}
            Component::ParentDir if !names.is_empty() => { names.pop(); directories.pop(); }
            _ => return Err("Ruta fuera del perfil de validación".into()),
        }
    }
    Ok(root.join(names.iter().collect::<PathBuf>()))
}

pub fn canonical_cwd(cwd: &Path, validation_root: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(root) = validation_root { return validation_directory(cwd, root); }
    let cwd = fs::canonicalize(cwd).map_err(|error| format!("Directorio no disponible: {error}"))?;
    let limit = validation_root.map(fs::canonicalize).transpose()
        .map_err(|error| format!("Perfil de validación no disponible: {error}"))?;
    if limit.as_ref().is_some_and(|root| !cwd.starts_with(root)) {
        return Err("Directorio fuera del perfil de validación".into());
    }
    let metadata = fs::metadata(&cwd).map_err(|error| format!("Directorio no disponible: {error}"))?;
    if !metadata.is_dir() {
        return Err("La ruta no es una carpeta".into());
    }
    Ok(cwd)
}

/// Call off the UI thread. An absent file falls back to cwd, as does an
/// unnamed buffer. I/O errors other than absence leave the request unresolved.
pub fn resolve(
    cwd: &Path,
    file: Option<&Path>,
    lsp_root: Option<&Path>,
    validation_root: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(root) = validation_root {
        let safe = |path: &Path| path.starts_with(root)
            && !path.components().any(|part| matches!(part, std::path::Component::ParentDir));
        if !safe(cwd) || file.is_some_and(|path| !safe(path)) || lsp_root.is_some_and(|path| !safe(path)) {
            return Err("Ruta fuera del perfil de validación".into());
        }
    }
    let cwd = canonical_cwd(cwd, validation_root)?;
    let limit = validation_root.map(fs::canonicalize).transpose()
        .map_err(|error| format!("Perfil de validación no disponible: {error}"))?;
    if limit.as_ref().is_some_and(|root| !cwd.starts_with(root)) {
        return Err("Directorio fuera del perfil de validación".into());
    }
    let file = match file {
        Some(file) => match fs::canonicalize(file) {
            Ok(path) => Some(path),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(format!("No se pudo resolver el archivo: {error}")),
        },
        None => None,
    };
    if limit.as_ref().zip(file.as_ref()).is_some_and(|(root, file)| !file.starts_with(root)) {
        return Err("Archivo fuera del perfil de validación".into());
    }
    if let (Some(file), Some(lsp)) = (&file, lsp_root) {
        let lsp = fs::canonicalize(lsp).map_err(|error| format!("Raíz LSP no disponible: {error}"))?;
        if file.starts_with(&lsp) && limit.as_ref().is_none_or(|root| lsp.starts_with(root)) {
            return Ok(lsp);
        }
    }
    let start = file.as_deref().and_then(Path::parent).unwrap_or(&cwd);
    for directory in start.ancestors() {
        if limit.as_ref().is_some_and(|root| !directory.starts_with(root)) { break; }
        for marker in [".git", "lua"] {
            match fs::symlink_metadata(directory.join(marker)) {
                Ok(_) => return Ok(directory.to_owned()),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(format!("No se pudo comprobar la raíz: {error}")),
            }
        }
    }
    Ok(cwd)
}
