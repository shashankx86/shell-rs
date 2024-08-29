use super::{register_command, Exec, RegisteredCommand};
use crate::cmds::flags::CommandFlags;
use crate::cmds::prompt::{confirm, Answer};
use crate::eval::{Scope, Value};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct Cp {
    flags: CommandFlags,
}

impl Cp {
    fn new() -> Self {
        let mut flags = CommandFlags::new();
        flags.add_flag('?', "help", "Display this help message");
        flags.add_flag('p', "progress", "Show progress bar");
        flags.add_flag('r', "recursive", "Copy directories recursively");
        flags.add_flag('f', "force", "Overwrite without prompting");
        flags.add_flag('i', "interactive", "Prompt before overwrite (default)");
        Cp { flags }
    }

    fn copy_file(&self, src: &Path, dst: &Path, pb: Option<&ProgressBar>) -> io::Result<()> {
        let mut src_file = File::open(src)?;
        let mut dst_file = File::create(dst)?;

        if let Some(pb) = pb {
            pb.set_message(
                src.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );
        }

        let mut buffer = [0; 8192];
        loop {
            let n = src_file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            dst_file.write_all(&buffer[..n])?;
            if let Some(pb) = pb {
                pb.inc(n as u64);
            }
        }

        Ok(())
    }

    fn get_source_files_and_size(&self, src: &Path) -> io::Result<(Vec<PathBuf>, u64)> {
        let mut total_size = 0;
        let mut files = Vec::new();

        fn wrap_error<E>(path: &Path, error: E) -> io::Error
        where
            E: std::fmt::Display,
        {
            io::Error::new(
                io::ErrorKind::Other,
                format!("{}: {}", path.display(), error),
            )
        }

        if src.is_dir() {
            for entry in fs::read_dir(src).map_err(|e| wrap_error(src, e))? {
                let entry = entry.map_err(|e| wrap_error(src, e))?;
                let path = entry.path();

                if path.is_symlink() {
                    continue; // TODO
                }

                if path.is_dir() {
                    let (mut sub_files, size) = self
                        .get_source_files_and_size(&path)
                        .map_err(|e| wrap_error(&path, e))?;
                    total_size += size;
                    files.append(&mut sub_files);
                } else {
                    let size = fs::metadata(&path).map_err(|e| wrap_error(&path, e))?.len();
                    total_size += size;
                    files.push(path);
                }
            }
        } else {
            total_size = fs::metadata(src).map_err(|e| wrap_error(src, e))?.len();
            files.push(src.to_path_buf());
        }

        Ok((files, total_size))
    }

    fn copy_files(
        &self,
        scope: &Rc<Scope>,
        src: &Path,
        dst: &Path,
        files: &[PathBuf],
        pb: Option<&ProgressBar>,
        interactive: &mut bool,
    ) -> io::Result<()> {
        for file in files {
            if scope.is_interrupted() {
                break;
            }
            let relative_path = file.strip_prefix(src).unwrap();
            let dst_path = dst.join(relative_path);

            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }

            if *interactive && dst.exists() {
                match confirm(format!("overwrite '{}'", dst_path.display()), true)? {
                    Answer::No => continue,
                    Answer::Quit => break,
                    Answer::Yes => {}
                    Answer::All => {
                        *interactive = false;
                        continue;
                    }
                }
            }

            self.copy_file(file, &dst_path, pb)?;
        }
        Ok(())
    }

    fn copy(
        &self,
        scope: &Rc<Scope>,
        src: &Path,
        dst: &Path,
        interactive: &mut bool,
        show_progress: bool,
        recursive: bool,
    ) -> io::Result<()> {
        if !recursive && src.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("omitting directory: {}", src.display()),
            ));
        }

        let (files, total_size) = self.get_source_files_and_size(src)?;

        let pb = if show_progress {
            let pb = ProgressBar::with_draw_target(Some(total_size), ProgressDrawTarget::stdout());
            pb.set_style(ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"));
            Some(pb)
        } else {
            None
        };

        if recursive {
            self.copy_files(scope, src, dst, &files, pb.as_ref(), interactive)?;
        } else {
            if *interactive
                && dst.exists()
                && confirm(format!("overwrite '{}'", dst.display()), false)? != Answer::Yes
            {
                return Ok(());
            }
            self.copy_file(src, dst, pb.as_ref())?;
        }

        if let Some(pb) = pb {
            pb.finish_with_message(if scope.is_interrupted() {
                "interrupted"
            } else {
                "done"
            });
        }
        Ok(())
    }
}

impl Exec for Cp {
    fn is_external(&self) -> bool {
        false
    }

    fn exec(&self, _name: &str, args: &Vec<String>, scope: &Rc<Scope>) -> Result<Value, String> {
        let mut flags = self.flags.clone();
        let args = flags.parse(args)?;

        if flags.is_present("help") {
            println!("Usage: cp [OPTIONS] SOURCE DEST");
            println!("Copy SOURCE to DEST, or multiple SOURCE(s) to DIRECTORY.");
            println!("\nOptions:");
            print!("{}", flags.help());
            return Ok(Value::success());
        }

        if args.len() != 2 {
            return Err("incorrect number of operands".to_string());
        }

        let show_progress = flags.is_present("progress");
        let recursive = flags.is_present("recursive");
        let mut interactive = !flags.is_present("force") || flags.is_present("interactive");

        let src = Path::new(&args[0]);
        let dst = Path::new(&args[1]);

        self.copy(scope, src, dst, &mut interactive, show_progress, recursive)
            .map_err(|e| format!("{}", e))?;

        println!();
        Ok(Value::success())
    }
}

#[ctor::ctor]
fn register() {
    register_command(RegisteredCommand {
        name: "cp".to_string(),
        inner: Rc::new(Cp::new()),
    });
}
