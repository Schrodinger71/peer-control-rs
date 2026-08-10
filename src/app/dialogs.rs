// состояние диалога добавления/редактирования узла
use crate::config::PeerInfo;

pub struct AddDialog {
    pub name: String,
    pub host: String,
    pub port: String,
    pub error: Option<String>,
    pub is_edit: bool,
    pub original_name: String,
}

impl AddDialog {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "5990".to_string(),
            error: None,
            is_edit: false,
            original_name: String::new(),
        }
    }

    pub fn edit(name: &str, peer: &PeerInfo) -> Self {
        Self {
            name: name.to_string(),
            host: peer.host.clone(),
            port: peer.port.to_string(),
            error: None,
            is_edit: true,
            original_name: name.to_string(),
        }
    }
}
