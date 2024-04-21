use rsheet_lib::cell_value::CellValue;
use rsheet_lib::command_runner::{CellArgument, CommandRunner};
use rsheet_lib::connect::{Manager, Reader, Writer};
use rsheet_lib::replies::Reply;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::mpsc::{self};
use std::sync::{Arc, Mutex};
use std::thread;

extern crate regex;

use log::info;

#[derive(Debug)]
pub struct CellDependency {
    expression: String,
    dependent_cells: HashSet<String>,
    included_variables: Vec<String>,
}

#[allow(clippy::type_complexity)]
pub fn handle_message(
    message: &str,
    cells: &Arc<Mutex<HashMap<String, CellValue>>>,
    dependencies: &Arc<Mutex<HashMap<String, CellDependency>>>,
    worker_sender: &std::sync::mpsc::Sender<(
        String,
        Arc<Mutex<HashMap<String, CellValue>>>,
        Arc<Mutex<HashMap<String, CellDependency>>>,
    )>,
) -> Option<Reply> {
    let parts: Vec<&str> = message.split_whitespace().collect();
    match parts[0] {
        "get" if parts.len() == 2 => {
            let cell_name = parts[1];
            let cells = cells.lock().unwrap();
            match cells.get(cell_name) {
                Some(CellValue::Error(err)) => Some(Reply::Error(err.to_string())),
                Some(value) => Some(Reply::Value(cell_name.to_string(), value.clone())),
                None => Some(Reply::Value(cell_name.to_string(), CellValue::None)),
            }
        }
        "set" if parts.len() >= 3 => {
            let cell_name = parts[1].to_string();
            let parts_var = parts[2..].to_vec();
            let parts_var_str = parts_var.join(" ");

            if parts_var_str.contains(&cell_name) {
                let mut cells = cells.lock().unwrap();
                cells.insert(
                    cell_name.clone(),
                    CellValue::Error("Cell is self-referential".to_string()),
                );
                return None;
            }

            let str_variables = CommandRunner::new(&parts_var_str).find_variables();
            let mut variables = HashMap::new();

            for var in str_variables.clone() {
                if var.contains('_') {
                    let cell_arg = parse_variable(&var, cells);
                    variables.insert(var, cell_arg);
                } else if let Some(value) = cells.lock().unwrap().get(&var) {
                    variables.insert(var, CellArgument::Value(value.clone()));
                } else {
                    variables.insert(var, CellArgument::Value(CellValue::None));
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

                let dependent_cells = dependencies
                    .entry(cell_name.clone())
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

            let cloned_cells = Arc::clone(cells);
            let cloned_dependencies = Arc::clone(dependencies);
            worker_sender
                .send((cell_name, cloned_cells, cloned_dependencies))
                .unwrap();

            None
        }
        _ => Some(Reply::Error("Invalid command".to_string())),
    }
}

fn parse_variable(variable: &str, cells: &Arc<Mutex<HashMap<String, CellValue>>>) -> CellArgument {
    let parts: Vec<&str> = variable.split('_').collect();

    let (start, end) = (parts[0], parts[1]);
    let start_col = start
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<String>();
    let start_row = start.chars().filter(|c| c.is_numeric()).collect::<String>();

    let end_col = end
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<String>();
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
            let cell_key = format!("{}{}", (col as u8 + b'A') as char, row + 1);
            let cell_value = cells.lock().unwrap().get(&cell_key).unwrap().clone();
            current_row.push(cell_value);
        }
        matrix.push(current_row);
    }

    CellArgument::Matrix(matrix)
}

pub fn multiple_dependency(
    cell_name: String,
    cloned_cells: Arc<Mutex<HashMap<String, CellValue>>>,
    cloned_dependencies: Arc<Mutex<HashMap<String, CellDependency>>>,
) {
    // println!("{:?}", cell_name);
    // println!("{:?}", cloned_cells.clone());
    // println!("{:?}", cloned_dependencies.clone());
    let mut variables_needed_update = HashSet::new();
    {
        let dependencies = cloned_dependencies.lock().unwrap();
        if let Some(dep) = dependencies.get(&cell_name) {
            for dependent_cell in &dep.dependent_cells {
                let cell_dep = dependencies.get(dependent_cell);
                let dependent_cell_expression = cell_dep
                    .as_ref()
                    .map(|cd| cd.expression.clone())
                    .unwrap_or_default();
                let included_variables = cell_dep
                    .as_ref()
                    .map(|cd| cd.included_variables.clone())
                    .unwrap_or_default();

                let variables = included_variables
                    .iter()
                    .map(|var| {
                        let value = cloned_cells
                            .lock()
                            .unwrap()
                            .get(var)
                            .cloned()
                            .unwrap_or(CellValue::None);
                        (var.clone(), CellArgument::Value(value))
                    })
                    .collect::<HashMap<_, _>>();

                let value = CommandRunner::new(&dependent_cell_expression).run(&variables);
                cloned_cells
                    .lock()
                    .unwrap()
                    .insert(dependent_cell.clone(), value);
                variables_needed_update.insert(dependent_cell.clone());
            }
        }
    }

    for var in variables_needed_update {
        multiple_dependency(
            var.clone(),
            cloned_cells.clone(),
            cloned_dependencies.clone(),
        );
    }
}

pub fn start_server<M>(mut manager: M) -> Result<(), Box<dyn Error>>
where
    M: Manager,
{
    let cells: Arc<Mutex<HashMap<String, CellValue>>> = Arc::new(Mutex::new(HashMap::new()));
    let dependencies: Arc<Mutex<HashMap<String, CellDependency>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let (worker_sender, worker_receiver) = mpsc::channel();

    thread::spawn(move || {
        while let Ok((cell_name, cloned_cells, cloned_dependencies)) = worker_receiver.recv() {
            multiple_dependency(cell_name, cloned_cells, cloned_dependencies);
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
                    s.spawn(move || loop {
                        let msg = match recv.read_message() {
                            Ok(msg) => msg,
                            Err(_) => return,
                        };
                        let reply = handle_message(
                            &msg,
                            &cloned_cells,
                            &cloned_dependencies,
                            &worker_sender_clone,
                        );
                        if let Some(reply_msg) = reply {
                            let _ = send.write_message(reply_msg);
                        }
                    });
                })
            }
            Err(_) => return Ok(()),
        }
    }
}
