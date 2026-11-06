//! Private, bounded tree clipboard. Never reads the system clipboard.
use crate::tree_transfer::Transfer;
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub project: usize,
    pub path: PathBuf,
    pub cut: bool,
}
#[derive(Default)]
pub struct Clipboard(pub Vec<Mark>);
impl Clipboard {
    /// Neo-tree y/x toggle each path, replacing its action when it differs.
    pub fn toggle(&mut self, project: usize, paths: &[PathBuf], cut: bool) -> Result<(), String> {
        let mut next = self.0.clone();
        for path in paths {
            if let Some(i) = next
                .iter()
                .position(|m| m.project == project && m.path == *path)
            {
                let previous = next.remove(i);
                if previous.cut == cut {
                    continue;
                }
            }
            next.push(Mark {
                project,
                path: path.clone(),
                cut,
            });
        }
        if next.len() > 128 {
            return Err("Límite de 128 entradas en el portapapeles del árbol".into());
        }
        self.0 = next;
        Ok(())
    }
    pub fn plan(
        &self,
        project: usize,
        root: &Path,
        parent: &Path,
    ) -> Result<Vec<Transfer>, String> {
        if self.0.is_empty() {
            return Err("Marcá primero archivos o carpetas con y/x".into());
        }
        self.0
            .iter()
            .map(|mark| {
                if mark.project != project {
                    return Err(
                        "El portapapeles contiene archivos de otro proyecto; Ctrl+r limpia".into(),
                    );
                }
                let name = mark.path.file_name().ok_or("Origen sin nombre")?;
                Ok(Transfer {
                    source: mark
                        .path
                        .strip_prefix(root)
                        .map_err(|_| "Origen fuera del proyecto")?
                        .to_path_buf(),
                    destination: parent
                        .join(name)
                        .strip_prefix(root)
                        .map_err(|_| "Destino fuera del proyecto")?
                        .to_path_buf(),
                    cut: mark.cut,
                })
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toggles_actions_and_preserves_bounded_batch_on_rejection() {
        let mut c = Clipboard::default();
        let paths = vec![PathBuf::from("/project/a"), PathBuf::from("/project/b")];
        c.toggle(0, &paths, false).unwrap();
        c.toggle(0, &paths[..1], true).unwrap();
        assert_eq!(c.0.len(), 2);
        assert!(c.0[1].cut);
        c.toggle(0, &paths[..1], true).unwrap();
        assert_eq!(c.0.len(), 1);
        let before = c.0.clone();
        assert!(
            c.toggle(
                0,
                &(0..129)
                    .map(|i| PathBuf::from(format!("/project/{i}")))
                    .collect::<Vec<_>>(),
                false
            )
            .is_err()
        );
        assert_eq!(c.0, before);
        assert!(
            c.plan(1, Path::new("/project"), Path::new("/project/dest"))
                .is_err()
        );
        let plan = c
            .plan(0, Path::new("/project"), Path::new("/project/dest"))
            .unwrap();
        assert_eq!(plan[0].destination, Path::new("dest/b"));
    }
}
