// Archivo sintético para probar el árbol y los pickers.
pub struct Workspace {
    pub project: String,
    pub terminal_windows: Vec<String>,
}

impl Workspace {
    pub fn focus_terminal(&self) -> Option<&str> {
        self.terminal_windows.first().map(String::as_str)
    }
}
