use rsheet_lib::cell_value::{self, CellValue};
use rsheet_lib::connect::{Manager, Reader, Writer};
use rsheet_lib::replies::Reply;

use std::collections::HashMap;
use std::error::Error;
use std::ops::Rem;
use std::sync::{Arc, Mutex};

use log::info;

pub struct SpreadsheetManager {
    cells: Arc<Mutex<HashMap<String, CellValue>>>,
}

impl SpreadsheetManager {
    pub fn new() -> Self {
        Self { cells: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn handle_message(&self, message: &str) -> Option<Reply> {
        let parts: Vec<&str> = message.trim().split_whitespace().collect();
        match parts[0] {
            "get" if parts.len() == 2 => {
                let cell_name = parts[1];
                let cells = self.cells.lock().unwrap();
                let cell_value = cells.get(cell_name);
                match cell_value {
                    Some(value) => Some(Reply::Value(cell_name.to_string(), value.clone())),
                    None => Some(Reply::Value(cell_name.to_string(), CellValue::None)),
                }
            },
            "set" if parts.len() >= 3 => {
                let cell_name = parts[1].to_string();
                let value_str = parts[2..].join(" ");
                let value = if let Ok(num) = value_str.parse::<i64>() {
                    CellValue::Int(num)
                } else {
                    CellValue::String(value_str)
                };
                let mut cells = self.cells.lock().unwrap();
                cells.insert(cell_name, value.clone());
                None
            },
            _ => Some(Reply::Error("Invalid command".to_string())),
        }
    }
}

pub fn start_server<M>(mut manager: M) -> Result<(), Box<dyn Error>>
where
    M: Manager,
{
    let spreadsheet_manager = SpreadsheetManager::new();
    let (mut recv, mut send) = manager.accept_new_connection().unwrap();
    loop {
        info!("Just got message");
        let msg = match recv.read_message() {
            Ok(msg) => {
                msg
            },
            Err(_) => {
                return Ok(());
            }
        };
        let reply = spreadsheet_manager.handle_message(&msg);
        match reply {
            Some(replay_msg) => {
                send.write_message(replay_msg)?;
            },
            None => {
            }
        }
    }
}
