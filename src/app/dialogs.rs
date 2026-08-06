// состояние диалога добавления узла

pub struct AddDialog {
    pub name: String,
    pub host: String,
    pub port: String,
    pub error: Option<String>,
}

impl AddDialog {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "5990".to_string(),
            error: None,
        }
    }
}
