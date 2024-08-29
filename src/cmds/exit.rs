use super::{register_command, Exec, ShellCommand};
use crate::eval::{Scope, Value};
use std::process;
use std::rc::Rc;

struct Exit;

impl Exec for Exit {
    fn exec(&self, _name: &str, args: &Vec<String>, _: &Rc<Scope>) -> Result<Value, String> {
        let exit_code = if args.len() > 0 {
            args[0]
                .parse::<i32>()
                .map_err(|_| "Invalid exit code. Please provide a valid integer.".to_string())?
        } else {
            0
        };

        process::exit(exit_code);
    }

    fn is_external(&self) -> bool {
        false
    }
}

#[ctor::ctor]
fn register() {
    register_command(ShellCommand {
        name: "exit".to_string(),
        inner: Rc::new(Exit),
    });
}
