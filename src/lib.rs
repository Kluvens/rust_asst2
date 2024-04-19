use clap::value_parser;
use rsheet_lib::cell_value::{self, CellValue};
use rsheet_lib::command_runner::{ CellArgument, CommandRunner };
use rsheet_lib::connect::{Manager, Reader, Writer};
use rsheet_lib::replies::Reply;

use std::collections::HashMap;
use std::error::Error;
use std::sync::mpsc::RecvError;
use std::sync::{mpsc, Arc, Mutex};
use std::{cell, result, thread, vec};

extern crate regex;
use regex::Regex;

use log::info;

pub fn handle_message(message: &str, cells: &Arc<Mutex<HashMap<String, CellValue>>>) -> Option<Reply> {
    let parts: Vec<&str> = message.trim().split_whitespace().collect();
    match parts[0] {
        "get" if parts.len() == 2 => {
            let cell_name = parts[1];
            let cells = cells.lock().unwrap();
            match cells.get(cell_name) {
                Some(value) => Some(Reply::Value(cell_name.to_string(), value.clone())),
                None => Some(Reply::Value(cell_name.to_string(), CellValue::None)),
            }
        },
        "set" if parts.len() >= 3 => {
            let cell_name = parts[1].to_string();
            let parts_var = parts[2..].to_vec();
            let parts_var_str = parts_var.join(" ");
            let mut variables = HashMap::new();

            for part in parts {
                if part.contains("_") {
                    if let Some(matrix_arg) = parse_matrix_variable(part, cells) {
                        variables.insert(remove_sum_expression(part), matrix_arg);
                    } else if let Some(vector_arg) = parse_matrix_variable(part, cells) {
                        variables.insert(remove_sum_expression(part), vector_arg);
                    }
                } else {
                    if let Ok(value) = part.parse::<i64>() {
                        variables.insert(remove_sum_expression(part), CellArgument::Value(CellValue::Int(value)));
                    } else if let Some(value) = cells.lock().unwrap().get(part) {
                        variables.insert(remove_sum_expression(part), CellArgument::Value(value.clone()));
                    }
                }
            }
            
            let value = CommandRunner::new(&remove_sum_expression(&parts_var_str)).run(&variables);

            {
                let mut cells = cells.lock().unwrap();
                cells.insert(cell_name, value);
            }
            None
        },
        _ => Some(Reply::Error("Invalid command".to_string())),
    }
}

fn parse_matrix_variable(variable: &str, cells: &Arc<Mutex<HashMap<String, CellValue>>>) -> Option<CellArgument> {
    todo!()
}

fn remove_sum_expression(input: &str) -> String {
    let re = Regex::new(r"sum\(([^)]+)\)").unwrap();
    re.replace_all(input, "$1").to_string()
}

pub fn start_server<M>(mut manager: M) -> Result<(), Box<dyn Error>>
where
    M: Manager,
{
    let cells: Arc<Mutex<HashMap<String, CellValue>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        match manager.accept_new_connection() {
            Ok(result) => {
                let (mut recv, mut send) = result;
                info!("Just got message");
                let cloned_cells = Arc::clone(&cells);
                thread::scope(|s| {
                    s.spawn(move || {
                        loop {
                            let msg = match recv.read_message() {
                                Ok(msg) => msg,
                                Err(_) => return,
                            };
                            let reply = handle_message(&msg, &cloned_cells);
                            if let Some(reply_msg) = reply {
                                send.write_message(reply_msg);
                            }
                        }
                    });
                })
            },
            Err(_) => return Ok(()),
        }
    }
}
