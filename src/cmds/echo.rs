use super::{register_command, Exec, ShellCommand};
use crate::eval::{Scope, Value};
use std::rc::Rc;
use crate::my_println;

struct Echo;

impl Exec for Echo {
    fn exec(&self, _name: &str, args: &Vec<String>, _: &Rc<Scope>) -> Result<Value, String> {
        my_println!("{}", args.join(" "))?;
        Ok(Value::success())
    }

    fn is_external(&self) -> bool {
        false
    }
}

#[ctor::ctor]
fn register() {
    register_command(ShellCommand {
        name: "echo".to_string(),
        inner: Rc::new(Echo),
    });
}
