use crate::eval::{Scope, Value};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::process::Command;
use std::rc::Rc;
use std::sync::Mutex;
use which::which;
mod cat;
mod cd;
mod clear;
mod echo;
mod env;
mod ls;

pub trait Exec {
    fn exec(&self, args: &Vec<String>, scope: &Rc<Scope>) -> Result<Value, String>;
}

#[derive(Clone)]
pub struct BuiltinCommand {
    name: String,
    inner: Rc<dyn Exec>,
}

impl Exec for BuiltinCommand {
    fn exec(&self, args: &Vec<String>, scope: &Rc<Scope>) -> Result<Value, String> {
        self.inner.exec(args, scope)
    }
}

unsafe impl Send for BuiltinCommand {}

lazy_static! {
    pub static ref COMMAND_REGISTRY: Mutex<HashMap<String, BuiltinCommand>> =
        Mutex::new(HashMap::new());
}

pub fn register_command(command: BuiltinCommand) {
    COMMAND_REGISTRY
        .lock()
        .unwrap()
        .insert(command.name.clone(), command);
}

pub fn get_command(name: &str) -> Option<BuiltinCommand> {
    let mut cmd = COMMAND_REGISTRY.lock().unwrap().get(name).cloned();
    if cmd.is_none() {
        if let Some(path) = locate_executable(name) {
            register_command(BuiltinCommand {
                name: name.to_string(),
                inner: Rc::new(External { path }),
            });
            cmd = COMMAND_REGISTRY.lock().unwrap().get(name).cloned();
        }
    }
    cmd
}

fn locate_executable(name: &str) -> Option<String> {
    match which(name) {
        Ok(path) => Some(path.to_string_lossy().to_string()),
        Err(_) => None,
    }
}

struct External {
    path: String,
}

impl Exec for External {
    fn exec(&self, args: &Vec<String>, scope: &Rc<Scope>) -> Result<Value, String> {
        let mut command = Command::new(&self.path);
        command.args(args);

        // Clear existing environment variables
        command.env_clear();

        // Set environment variables from the scope
        for (key, variable) in scope.vars.borrow().iter() {
            command.env(key, variable.value().to_string());
        }

        match command.spawn() {
            Ok(mut child) => match child.wait() {
                Ok(status) => {
                    if let Some(code) = status.code() {
                        Ok(Value::Int(code as _))
                    } else {
                        Ok(Value::Str("".to_owned()))
                    }
                }
                Err(e) => Err(format!("Failed to wait on child process: {}", e)),
            },
            Err(e) => Err(format!("Failed to execute command: {}", e)),
        }
    }
}
