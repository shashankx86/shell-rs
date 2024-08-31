use super::{register_command, Exec, ShellCommand};
use crate::cmds::flags::CommandFlags;
use crate::eval::{Scope, Value};
use crate::my_println;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead};
use std::rc::Rc;

enum Mode {
    Cat,
    Head,
    Tail,
}

struct CatHeadTail {
    flags: CommandFlags,
    mode: Mode,
}

impl CatHeadTail {
    fn new(mode: Mode) -> Self {
        let mut flags = CommandFlags::new();
        flags.add_flag('n', "number", "Number all output lines");

        if matches!(mode, Mode::Head | Mode::Tail) {
            flags.add_value_flag('l', "lines", "Specify the number of lines to output");
        }
        flags.add_flag('?', "help", "Display this help message");
        CatHeadTail { flags, mode }
    }

    fn mode_specific_help(&self) -> &str {
        match self.mode {
            Mode::Cat => "Concatenate FILE(s) to standard output.",
            Mode::Head => "Output the first part of files.",
            Mode::Tail => "Output the last part of files.",
        }
    }
}

impl Exec for CatHeadTail {
    fn exec(&self, name: &str, args: &Vec<String>, _: &Rc<Scope>) -> Result<Value, String> {
        let mut flags = self.flags.clone();
        let filenames = flags.parse(args)?;

        if flags.is_present("help") {
            println!("Usage: {} [OPTION]... [FILE]...", name);
            println!("{}", self.mode_specific_help());
            println!("\nOptions:");
            print!("{}", flags.help());
            return Ok(Value::success());
        }

        let line_numbers: bool = flags.is_present("number");
        let lines = flags
            .get_value("lines")
            .map(|v| v.parse::<usize>().map_err(|e| e.to_string()))
            .unwrap_or(Ok(10))?;

        if filenames.is_empty() {
            let stdin = io::stdin();
            process_input(&mut stdin.lock(), &self.mode, line_numbers, lines)?;
        } else {
            for filename in &filenames {
                let file = File::open(&filename).map_err(|e| e.to_string())?;
                let mut buf_reader = io::BufReader::new(file);
                process_input(&mut buf_reader, &self.mode, line_numbers, lines)?;
            }
        }
        Ok(Value::success())
    }

    fn is_external(&self) -> bool {
        false
    }
}

fn process_input<R: BufRead>(
    reader: &mut R,
    mode: &Mode,
    line_numbers: bool,
    lines: usize,
) -> Result<(), String> {
    match mode {
        Mode::Cat => print_all(reader, line_numbers),
        Mode::Head => print_head(reader, line_numbers, lines),
        Mode::Tail => print_tail(reader, line_numbers, lines),
    }
}

fn print_all<R: BufRead>(reader: &mut R, line_numbers: bool) -> Result<(), String> {
    for (i, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if line_numbers {
            my_println!("{:>6}: {}", i + 1, line)?;
        } else {
            my_println!("{}", line)?;
        }
    }
    Ok(())
}

fn print_head<R: BufRead>(reader: &mut R, line_numbers: bool, lines: usize) -> Result<(), String> {
    for (i, line) in reader.lines().enumerate().take(lines) {
        let line = line.map_err(|e| e.to_string())?;
        if line_numbers {
            my_println!("{:>6}: {}", i + 1, line)?;
        } else {
            my_println!("{}", line)?;
        }
    }
    Ok(())
}

fn print_tail<R: BufRead>(reader: &mut R, line_numbers: bool, lines: usize) -> Result<(), String> {
    let mut buffer = VecDeque::with_capacity(lines);

    for (i, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if buffer.len() == lines {
            buffer.pop_front();
        }
        buffer.push_back((i, line));
    }
    for (i, line) in buffer {
        if line_numbers {
            my_println!("{:>6}: {}", i + 1, line)?;
        } else {
            my_println!("{}", line)?;
        }
    }
    Ok(())
}

#[ctor::ctor]
fn register() {
    register_command(ShellCommand {
        name: "cat".to_string(),
        inner: Rc::new(CatHeadTail::new(Mode::Cat)),
    });
    register_command(ShellCommand {
        name: "head".to_string(),
        inner: Rc::new(CatHeadTail::new(Mode::Head)),
    });
    register_command(ShellCommand {
        name: "tail".to_string(),
        inner: Rc::new(CatHeadTail::new(Mode::Tail)),
    });
}
