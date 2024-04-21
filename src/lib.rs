use clap::value_parser;
use rsheet_lib::cell_value::{self, CellValue};
use rsheet_lib::cells::column_name_to_number;
use rsheet_lib::command_runner::{CellArgument, CommandRunner};
use rsheet_lib::connect::{Manager, Reader, Writer};
use rsheet_lib::replies::Reply;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::mpsc::{self, channel, RecvError};
use std::sync::{Arc, Mutex};
use std::{cell, result, thread, vec};

extern crate regex;
use regex::Regex;

use log::info;

#[derive(Debug)]
pub struct CellDependency {
    expression: String,
    dependent_cells: HashSet<String>,
    included_variables: Vec<String>,
}

pub fn handle_message(
    message: &str, 
    cells: &Arc<Mutex<HashMap<String, CellValue>>>, 
    dependencies: &Arc<Mutex<HashMap<String, CellDependency>>>,
    worker_sender: &std::sync::mpsc::Sender<(String, Arc<Mutex<HashMap<String, CellValue>>>, Arc<Mutex<HashMap<String, CellDependency>>>)>,
) -> Option<Reply> {
    let parts: Vec<&str> = message.trim().split_whitespace().collect();
    match parts[0] {
        "get" if parts.len() == 2 => {
            let cell_name = parts[1];
            let cells = cells.lock().unwrap();
            match cells.get(cell_name) {
                Some(CellValue::Error(err)) => Some(Reply::Value(cell_name.to_string(), CellValue::Error(err.to_string()))),
                Some(value) => Some(Reply::Value(cell_name.to_string(), value.clone())),
                None => Some(Reply::Value(cell_name.to_string(), CellValue::None)),
            }
        },
        "set" if parts.len() >= 3 => {
            let cell_name = parts[1].to_string();
            let parts_var = parts[2..].to_vec();
            let parts_var_str = parts_var.join(" ");
            let str_variables = CommandRunner::new(&parts_var_str).find_variables();
            let mut variables = HashMap::new();

            for var in str_variables.clone() {
                if var.contains("_") {
                    let cell_arg = parse_variable(&var, cells);
                    variables.insert(var, cell_arg);
                } else {
                    if let Some(value) = cells.lock().unwrap().get(&var) {
                        variables.insert(var, CellArgument::Value(value.clone()));
                    } else {
                        variables.insert(var, CellArgument::Value(CellValue::None));
                    }
                }
            }
            
            let value = CommandRunner::new(&parts_var_str).run(&variables);

            {
                let mut cells = cells.lock().unwrap();
                cells.insert(cell_name.clone(), value);
            }

            {
                let mut dependencies = dependencies.lock().unwrap();

                for var in str_variables.clone() {
                    let entry = dependencies.entry(var).or_insert(CellDependency {
                        expression: String::new(),
                        dependent_cells: HashSet::new(),
                        included_variables: Vec::new(),
                    });
                    entry.dependent_cells.insert(cell_name.clone());
                }

                let dependent_cells = dependencies.entry(cell_name.clone())
                    .or_insert_with(|| CellDependency {
                        expression: parts_var_str.clone(),
                        dependent_cells: HashSet::new(),
                        included_variables: str_variables.clone(),
                    })
                    .dependent_cells
                    .clone();

                dependencies.insert(
                    cell_name.clone(),
                    CellDependency {
                        expression: parts_var_str.clone(),
                        dependent_cells,
                        included_variables: str_variables,
                    },
                );
            }

            let cloned_cells = Arc::clone(&cells);
            let cloned_dependencies = Arc::clone(&dependencies);
            worker_sender.send((cell_name, cloned_cells, cloned_dependencies)).unwrap();

            None
        },
        _ => Some(Reply::Error("Invalid command".to_string())),
    }
}

fn parse_variable(variable: &str, cells: &Arc<Mutex<HashMap<String, CellValue>>>) -> CellArgument {
    let parts: Vec<&str> = variable.split('_').collect();

    let (start, end) = (parts[0], parts[1]);
    let start_col = start.chars().filter(|c| c.is_alphabetic()).collect::<String>();
    let start_row = start.chars().filter(|c| c.is_numeric()).collect::<String>();

    let end_col = end.chars().filter(|c| c.is_alphabetic()).collect::<String>();
    let end_row = end.chars().filter(|c| c.is_numeric()).collect::<String>();

    let start_col_idx = column_name_to_number(&start_col);
    let end_col_idx = column_name_to_number(&end_col);

    let start_row_idx: usize = start_row.parse::<usize>().expect("Invalid row number") - 1;
    let end_row_idx: usize = end_row.parse::<usize>().expect("Invalid row number") - 1;

    if start_col == end_col {
        let mut vector = Vec::new();

        for row in start_row_idx..=end_row_idx {
            let cell_key = format!("{}{}", start_col, row + 1);
            let cell_value = cells.lock().unwrap().get(&cell_key).unwrap().clone();
            vector.push(cell_value);
        }

        return CellArgument::Vector(vector);
    }

    let mut matrix = Vec::new();

    for row in start_row_idx..=end_row_idx {
        let mut current_row = Vec::new();
        for col in start_col_idx..=end_col_idx {
            let cell_key = format!("{}{}", (col as u8 + 'A' as u8) as char, row + 1);
            let cell_value = cells.lock().unwrap().get(&cell_key).unwrap().clone();
            current_row.push(cell_value);
        }
        matrix.push(current_row);
    }

    CellArgument::Matrix(matrix)
}

pub fn start_server<M>(mut manager: M) -> Result<(), Box<dyn Error>>
where
    M: Manager,
{
    let cells: Arc<Mutex<HashMap<String, CellValue>>> = Arc::new(Mutex::new(HashMap::new()));
    let dependencies: Arc<Mutex<HashMap<String, CellDependency>>> = Arc::new(Mutex::new(HashMap::new()));

    let (worker_sender, worker_receiver): (
        std::sync::mpsc::Sender<(String, Arc<Mutex<HashMap<String, CellValue>>>, Arc<Mutex<HashMap<String, CellDependency>>>)>,
        std::sync::mpsc::Receiver<(String, Arc<Mutex<HashMap<String, CellValue>>>, Arc<Mutex<HashMap<String, CellDependency>>>)>,
    ) = mpsc::channel();

    thread::spawn(move || {
        loop {
            match worker_receiver.recv() {
                Ok((cell_name, cloned_cells, cloned_dependencies)) => {
                    let dependencies = cloned_dependencies.lock().unwrap();
                    if let Some(dep) = dependencies.get(&cell_name) {
                        for dependent_cell in &dep.dependent_cells {
                            let dependent_cell_expression = dependencies.get(dependent_cell).unwrap().expression.clone();
                            let included_variables = dependencies.get(dependent_cell).unwrap().included_variables.clone();
                            let mut variables = HashMap::new();
                            for var in included_variables {
                                if let Some(value) = cloned_cells.lock().unwrap().get(&var) {
                                    variables.insert(var.clone(), CellArgument::Value(value.clone()));
                                } else {
                                    variables.insert(var.clone(), CellArgument::Value(CellValue::None));
                                }
                            }
                            let value = CommandRunner::new(&dependent_cell_expression).run(&variables);
                            let mut cells = cloned_cells.lock().unwrap();
                            cells.insert(dependent_cell.clone(), value);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        match manager.accept_new_connection() {
            Ok(result) => {
                let (mut recv, mut send) = result;
                info!("Just got message");
                let cloned_cells = Arc::clone(&cells);
                let cloned_dependencies = Arc::clone(&dependencies);
                let worker_sender_clone = worker_sender.clone();
                thread::scope(|s| {
                    s.spawn(move || {
                        loop {
                            let msg = match recv.read_message() {
                                Ok(msg) => msg,
                                Err(_) => return,
                            };
                            let reply = handle_message(&msg, &cloned_cells, &cloned_dependencies, &worker_sender_clone);
                            if let Some(reply_msg) = reply {
                                send.write_message(reply_msg);
                            }
                        }
                    });
                })
            }
            Err(_) => return Ok(()),
        }
    }
}
