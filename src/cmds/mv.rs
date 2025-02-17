use super::{register_command, Exec, ShellCommand};
use crate::cmds::flags::CommandFlags;
use crate::eval::{Scope, Value};
use crate::prompt::{confirm, Answer};
use std::fs;
use std::path::Path;
use std::rc::Rc;

struct Mv {
    flags: CommandFlags,
}

impl Mv {
    fn new() -> Self {
        let mut flags = CommandFlags::new();
        flags.add_flag('?', "help", "Display this help message");
        flags.add_flag('f', "force", "Do not prompt before overwriting");
        flags.add_flag('i', "interactive", "Prompt before overwriting files");
        Mv { flags }
    }
}

impl Exec for Mv {
    fn is_external(&self) -> bool {
        false
    }

    fn exec(&self, _name: &str, args: &Vec<String>, scope: &Rc<Scope>) -> Result<Value, String> {
        let mut flags = self.flags.clone();
        let args = flags.parse(args)?;

        if flags.is_present("help") {
            println!("Usage: mv [OPTIONS] SOURCE DEST");
            println!("Move (rename) SOURCE to DESTination.");
            println!("\nOptions:");
            print!("{}", flags.help());
            return Ok(Value::success());
        }

        if args.is_empty() {
            return Err("Missing source and destination".to_string());
        }
        if args.len() < 2 {
            return Err("Missing destination".to_string());
        }
        if args.len() > 2 {
            return Err("Extraneous argument".to_string());
        }

        let src = Path::new(&args[0]);
        let dest = Path::new(&args[1]);

        let interactive = !flags.is_present("force") || flags.is_present("interactive");

        let final_dest = if dest.is_dir() {
            dest.join(src.file_name().ok_or("Invalid source filename")?)
        } else {
            dest.to_path_buf()
        };

        if src == final_dest {
            return Err("Source and destination are the same".to_string());
        }

        if final_dest.exists()
            && interactive
            && confirm(
                format!("Overwrite '{}'", final_dest.display()),
                scope,
                false,
            )
            .map_err(|e| e.to_string())?
                != Answer::Yes
        {
            return Ok(Value::success());
        }

        fs::rename(src, final_dest).map_err(|e| e.to_string())?;

        Ok(Value::success())
    }
}

#[ctor::ctor]
fn register() {
    register_command(ShellCommand {
        name: "mv".to_string(),
        inner: Rc::new(Mv::new()),
    });
}
