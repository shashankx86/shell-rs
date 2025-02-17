use crate::cmds::{get_command, Exec, ShellCommand};
use crate::prompt::{confirm, Answer};
use gag::{BufferRedirect, Gag, Redirect};
use glob::glob;
use regex::Regex;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::fmt::{self, Debug};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::Ordering::SeqCst;

pub const KEYWORDS: [&str; 8] = [
    "BREAK", "CONTINUE", "ELSE", "FOR", "IF", "IN", "QUIT", "WHILE",
];

#[derive(Clone, Debug, PartialEq)]
enum Op {
    And,
    Append,
    Assign,
    Div,
    Equals,
    Gt,
    Gte,
    IntDiv,
    Minus,
    Mod,
    Mul,
    Lt,
    Lte,
    Not,
    NotEquals,
    Or,
    Pipe,
    Plus,
    Write,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::And => write!(f, "&&"),
            Op::Append => write!(f, "=>>"),
            Op::Assign => write!(f, "="),
            Op::Div => write!(f, "/"),
            Op::Equals => write!(f, "=="),
            Op::Gt => write!(f, ">"),
            Op::Gte => write!(f, ">="),
            Op::IntDiv => write!(f, "//"),
            Op::Minus => write!(f, "-"),
            Op::Mod => write!(f, "%"),
            Op::Mul => write!(f, "*"),
            Op::Lt => write!(f, "<"),
            Op::Lte => write!(f, "<="),
            Op::Not => write!(f, "!"),
            Op::NotEquals => write!(f, "!="),
            Op::Or => write!(f, "||"),
            Op::Pipe => write!(f, "|"),
            Op::Plus => write!(f, "+"),
            Op::Write => write!(f, "=>"),
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd)]
enum Priority {
    VeryLow,
    Low,
    High,
}

impl Op {
    fn priority(&self) -> Priority {
        match &self {
            Op::Assign | Op::Pipe => Priority::VeryLow,
            Op::And
            | Op::Append
            | Op::Or
            | Op::Gt
            | Op::Gte
            | Op::Lt
            | Op::Lte
            | Op::Not
            | Op::NotEquals
            | Op::Minus
            | Op::Plus
            | Op::Write => Priority::Low,
            _ => Priority::High,
        }
    }

    fn is_unary_ok(&self) -> bool {
        return matches!(&self, Op::Minus | Op::Not);
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    End,
    Keyword(String),
    Literal((String, bool)),
    Operator(Op),
    LeftParen,
    RightParen,
    Semicolon,
}

/// Location information for error reporting
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Location {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[{}:{}]", self.line, self.col)
    }
}

/// Trait for objects with location info.
trait HasLocation {
    fn loc(&self) -> Location;
}

impl Location {
    fn new() -> Self {
        Self { line: 1, col: 1 }
    }

    fn next_line(&mut self) {
        self.line += 1;
        self.col = 0;
    }
}

macro_rules! derive_has_location {
    ($type:ty) => {
        impl HasLocation for $type {
            fn loc(&self) -> Location {
                self.loc
            }
        }
    };
}

/// Wrap the status (result) of a command execution.
/// The idea is to delay dealing with errors: if the status is checked (by
/// being evaluated as bool), then the error (if any) is treated as handled.
/// If the Status object is never checked, the error is returned by the eval
/// of group containing the command expression.
#[derive(Debug, PartialEq)]
pub struct Status {
    checked: bool,
    cmd: String,
    negated: bool,
    result: EvalResult<Value>,
    scope: Rc<Scope>,
}

fn collect_var(scope: &Rc<Scope>, var_name: &str, info: String) {
    match &scope.lookup_local(var_name) {
        Some(v) => {
            v.assign(Value::Str(format!("{}\n{}", v.to_string(), info)));
        }
        _ => {
            scope.insert(var_name.to_string(), Value::Str(info));
        }
    }
}

impl Status {
    fn new(cmd: String, result: &EvalResult<Value>, scope: &Rc<Scope>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            checked: false,
            cmd,
            negated: false,
            result: result.clone(),
            scope: Rc::clone(&scope),
        }))
    }

    fn as_bool(&mut self, scope: &Rc<Scope>) -> bool {
        if let Err(e) = &self.result {
            collect_var(scope, "__errors", format!("{}: {}", self.cmd, e.message));
        }

        self.checked = true;

        if self.negated {
            self.result.is_err()
        } else {
            self.result.is_ok()
        }
    }

    fn check_result(result: EvalResult<Value>) -> EvalResult<Value> {
        match &result {
            Ok(Value::Stat(status)) => {
                if !status.borrow().checked {
                    status.borrow_mut().checked = true;
                    return status.borrow().result.clone();
                }
            }
            _ => {} // Propagate the error
        }
        result
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "result: {:?} checked: {}", &self.result, self.checked)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Real(f64),
    Str(String),
    Stat(Rc<RefCell<Status>>),
}

impl Default for Value {
    fn default() -> Self {
        Value::Str(String::default())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            Value::Int(v) => {
                write!(f, "{}", v)
            }
            Value::Real(v) => {
                write!(f, "{}", v)
            }
            Value::Str(s) => {
                write!(f, "{}", s)
            }
            Value::Stat(s) => {
                write!(f, "{}", s.borrow())
            }
        }
    }
}

impl FromStr for Value {
    type Err = EvalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(i) = s.parse::<i64>() {
            Ok(Value::Int(i))
        } else if let Ok(f) = s.parse::<f64>() {
            Ok(Value::Real(f))
        } else {
            Ok(Value::Str(s.to_string()))
        }
    }
}

impl Value {
    pub fn success() -> Self {
        Value::Int(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Jump {
    Break(Value),
    Continue(Value),
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvalError {
    pub loc: Location,
    pub message: String,
    jump: Option<Jump>,
}

impl EvalError {
    fn new(loc: Location, message: String) -> Self {
        Self {
            loc,
            message,
            jump: None,
        }
    }

    pub fn show(&self, input: &String) {
        // TODO: deal with wrap around
        let line = self.loc.line as usize;
        let col = self.loc.col as usize;

        // Get the problematic line from the input
        let lines: Vec<&str> = input.lines().collect();
        let error_line = lines.get(line - 1).unwrap_or(&"");

        // Create the error indicator
        let indicator = "-".repeat(col - 1) + "^";

        eprintln!("Error at line {}, column {}:", line, col);
        eprintln!("{}", error_line);
        eprintln!("{}", indicator);
        eprintln!("{}", self.message);
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.loc, self.message)
    }
}

pub type EvalResult<T = ()> = std::result::Result<T, EvalError>;

trait Eval {
    fn eval(&self) -> EvalResult<Value>;
}

fn error<S: HasLocation, R>(source: &S, message: &str) -> EvalResult<R> {
    Err(EvalError::new(source.loc(), message.to_string()))
}

/// Non-terminal AST node.
trait ExprNode {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult;
}

struct Parser<I: Iterator<Item = char>> {
    chars: Peekable<I>,
    loc: Location,
    prev_loc: Location,
    comment: bool,
    escaped: bool,
    in_quotes: bool,
    expect_else_expr: bool,
    empty: Rc<Expression>,
    current_expr: Rc<Expression>,
    scope: Rc<Scope>,
    expr_stack: Vec<Rc<Expression>>,
    scope_stack: Vec<Rc<Scope>>,
    group: Rc<Expression>,
    group_stack: Vec<Rc<Expression>>,
    globbed_tokens: Vec<String>,
    text: String,
    quoted: bool,
}

impl<I: Iterator<Item = char>> HasLocation for Parser<I> {
    fn loc(&self) -> Location {
        self.loc
    }
}

/// Tokenizer helpers
macro_rules! check_text {
    ($self:expr, $tok:expr) => {
        if !$self.text.is_empty() {
            $tok = $self.glob_literal()?;
            break;
        }
    };
}
macro_rules! token {
    // Single character token
    ($self:expr, $tok:expr, $single_token:expr) => {{
        check_text!($self, $tok);
        $self.next();
        $tok = $single_token;
    }};

    // Double character token only
    ($self:expr, $tok:expr, $second:expr, $double_token:expr) => {{
        check_text!($self, $tok);
        $self.next();
        if let Some(&next_c) = $self.chars.peek() {
            if next_c == $second {
                $self.next();
                $tok = $double_token;
                continue;
            }
        }
    }};

    // Single or double character token
    ($self:expr, $tok:expr,$second:expr, $single_token:expr, $double_token:expr) => {{
        check_text!($self, $tok);
        $self.next();
        if let Some(&next_c) = $self.chars.peek() {
            if next_c == $second {
                $self.next();
                $tok = $double_token;
                continue;
            }
        }
        $tok = $single_token;
    }};
}

impl<T> Parser<T>
where
    T: Iterator<Item = char>,
{
    fn new(input: T, interp_scope: &Rc<Scope>) -> Self {
        let empty = Rc::new(Expression::Empty);
        let loc = Location::new();

        // Create a child scope of the interpreter scope; the interp scope contains
        // the environmental vars, which should not be cleared between evaluations.
        let scope = Scope::new(Some(Rc::clone(&interp_scope)));

        Self {
            chars: input.peekable(),
            loc,
            prev_loc: loc,
            comment: false,
            escaped: false,
            in_quotes: false,
            expect_else_expr: false,
            empty: Rc::clone(&empty),
            current_expr: Rc::clone(&empty),
            scope: Rc::clone(&scope),
            expr_stack: Vec::new(),
            scope_stack: Vec::new(),
            group: new_group(loc, &scope),
            group_stack: Vec::new(),
            globbed_tokens: Vec::new(),
            text: String::new(),
            quoted: false,
        }
    }

    fn empty(&self) -> Rc<Expression> {
        Rc::clone(&self.empty)
    }

    fn is_delimiter(&self, tok: &str, c: char) -> bool {
        // Forward slashes and dashes need special handling, since they occur in
        // paths and command line options; it is unreasonable to require quotes.
        if "/-*".contains(c) {
            if tok.is_empty() {
                return !self.group.is_args()
                    && !self.current_expr.is_cmd()
                    && !self.current_expr.is_empty();
            }
            match parse_value(tok, self.loc, &self.scope) {
                Ok(Value::Int(_)) | Ok(Value::Real(_)) => true,
                _ => false,
            }
        } else {
            const DELIMITERS: &str = " \t\n\r()+=;|&<>";
            DELIMITERS.contains(c)
        }
    }

    fn next(&mut self) {
        self.loc.col += 1;
        self.chars.next();
    }

    fn glob_literal(&mut self) -> EvalResult<Token> {
        // This function should not be called if globbed_tokens are not depleted.
        assert!(self.globbed_tokens.is_empty());

        if !self.quoted {
            let upper = self.text.to_uppercase();
            for &keyword in &KEYWORDS {
                if keyword == upper {
                    return Ok(Token::Keyword(upper));
                }
            }

            if self.text.starts_with("~") {
                if let Some(v) = self.scope.lookup("HOME") {
                    self.text = format!("{}{}", v.value().to_string(), &self.text[1..]);
                }
            }

            match glob(&self.text) {
                Ok(paths) => {
                    self.globbed_tokens = paths
                        .filter_map(Result::ok)
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();

                    if !self.globbed_tokens.is_empty() {
                        return Ok(Token::Literal((self.globbed_tokens.remove(0), false)));
                    }
                }
                Err(_) => {} // Ignore glob errors and treat as literal
            }
        }
        Ok(Token::Literal((self.text.clone(), self.quoted)))
    }

    #[rustfmt::skip]
    pub fn next_token(&mut self) -> EvalResult<Token> {

        if !self.globbed_tokens.is_empty() {
            return Ok(Token::Literal((self.globbed_tokens.remove(0), false)));
        }

        let mut tok = Token::End;
        self.quoted = false;
        self.text.clear();

        while let Some(c) = self.chars.peek().cloned() {
            if tok != Token::End {
                break;
            }

            if c == '\n' {
                self.loc.next_line();
                self.comment = false;
                self.next();
                continue;
            }
            if self.comment {
                self.next();
                continue;
            }
            match c {
                '#' => { self.comment = true; self.next(); }
                '%' => token!(self, tok, Token::Operator(Op::Mod)),
                '(' => token!(self, tok, Token::LeftParen),
                ')' => token!(self, tok, Token::RightParen),
                ';' => token!(self, tok, Token::Semicolon),
                '+' => token!(self, tok, Token::Operator(Op::Plus)),
                '&' => token!(self, tok, '&', Token::Operator(Op::And)),
                '|' => token!(self, tok, '|', Token::Operator(Op::Pipe), Token::Operator(Op::Or)),
                '!' => token!(self, tok, '=', Token::Operator(Op::Not), Token::Operator(Op::NotEquals)),
                '*' => {
                    if !self.is_delimiter(&self.text, c) {
                        self.text.push(c);
                    } else {
                        check_text!(self, tok);
                        tok = Token::Operator(Op::Mul)
                    }
                    self.next();
                }
                '<' => token!(self, tok, '=', Token::Operator(Op::Lt), Token::Operator(Op::Lte)),
                '>' => token!(self, tok, '=', Token::Operator(Op::Gt), Token::Operator(Op::Gte)),
                '=' => {
                    check_text!(self, tok);
                    self.next();
                    if let Some(&next_c) = self.chars.peek() {
                        if next_c == '=' {
                            self.next();
                            tok = Token::Operator(Op::Equals);
                            continue;
                        }
                        if next_c == '>' {
                            self.next();
                            if let Some(&next_c) = self.chars.peek() {
                                if next_c == '>' {
                                    self.next();
                                    tok = Token::Operator(Op::Append);
                                    continue;
                                }
                            }
                            tok = Token::Operator(Op::Write);
                            continue;
                        }
                        tok = Token::Operator(Op::Assign);
                    }
                },
                '-' => {
                    if !self.is_delimiter(&self.text, c) {
                        self.text.push(c);
                    } else {
                        check_text!(self, tok);
                        tok = Token::Operator(Op::Minus);
                    }
                    self.next();
                }
                '/' => {
                    // Treat forward slashes as chars in arguments to commands, to avoid quoting file paths.
                    if !self.is_delimiter(&self.text, c) {
                        self.text.push(c);
                        self.next();
                    } else {
                        check_text!(self, tok);
                        token!(self, tok, '/', Token::Operator(Op::Div), Token::Operator(Op::IntDiv));
                    }
                }
                _ => {
                    if c.is_whitespace() {
                        self.next();
                        if !self.text.is_empty() {
                            break;
                        }
                        continue;
                    }

                    while let Some(&next_c) = self.chars.peek() {
                        if self.escaped {
                            match next_c {
                                'n' => self.text.push('\n'),
                                't' => self.text.push('\t'),
                                'r' => self.text.push('\r'),
                                _ => self.text.push(next_c),
                            }
                            self.next();
                            self.escaped = false;
                        } else if next_c == '\\' {
                            self.next();
                            if self.in_quotes {
                                // Escapes only work inside quotes, to avoid
                                // issues with path delimiters under Windows
                                self.escaped = true;
                            } else {
                                self.text.push('\\');
                            }
                        } else if next_c == '"' {
                            self.quoted = true;
                            self.in_quotes ^= true;
                            self.next();
                        } else {
                            if self.in_quotes || !self.is_delimiter(&self.text, next_c) {
                                self.text.push(next_c);
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }

                    if !self.text.is_empty() || self.quoted {
                        assert!(self.text != "-" && self.text != "/");

                        tok = self.glob_literal()?;
                        self.text.clear();
                    }
                }
            }
        }
        if self.in_quotes {
            return error(self, "Unbalanced quotes");
        }

        // Check for partial token, handle special cases such as single fwd slash.
        if tok == Token::End && !self.text.is_empty() {
            if self.text == "-" && self.current_expr.is_number() {
                tok = Token::Operator(Op::Minus);
            } else if self.text == "/" && self.current_expr.is_number() {
                tok = Token::Operator(Op::Div);
            } else {
                tok = self.glob_literal()?;
            }
        }

        Ok(tok)
    }

    /// Add an expression to the AST.
    fn add_expr(&mut self, expr: &Rc<Expression>) -> EvalResult {
        assert!(!expr.is_empty());

        self.prev_loc = self.loc;

        if self.expect_else_expr {
            self.current_expr = self.expr_stack.pop().unwrap();
            self.expect_else_expr = false;
        }

        let ref current = *self.current_expr;

        if current.is_complete() {
            if let Expression::Args(a) = &*self.group {
                a.borrow_mut().add_child(&self.current_expr)?;
                self.current_expr = Rc::clone(&expr);
                return Ok(());
            } else {
                return error(&**expr, "Unexpected expression");
            }
        }

        match current {
            Expression::Args(e) => e.borrow_mut().add_child(expr),
            Expression::Bin(e) => e.borrow_mut().add_child(expr),
            Expression::Branch(e) => e.borrow_mut().add_child(expr),
            Expression::Cmd(e) => e.borrow_mut().add_child(expr),
            Expression::Empty => {
                self.current_expr = Rc::clone(expr);
                Ok(())
            }
            Expression::Group(e) => e.borrow_mut().add_child(expr),
            Expression::For(e) => e.borrow_mut().add_child(expr),
            Expression::Leaf(_) => error(self, "Unexpected expression after literal"),
            Expression::Loop(e) => e.borrow_mut().add_child(expr),
        }
    }

    fn close_group(group: &Rc<Expression>) {
        match &**group {
            Expression::Args(g) => {
                g.borrow_mut().closed = true;
            }
            Expression::Group(g) => {
                g.borrow_mut().closed = true;
            }
            _ => {
                dbg!(&group);
                panic!("Expecting group expression");
            }
        }
    }

    fn pop_binary_ops(&mut self, end_statement: bool) -> EvalResult {
        while let Some(stack_top) = self.expr_stack.last() {
            // If the expression on the top of the expression stack is a binary
            // expression, pop it; add current expression to it; then make it the
            // new current expression.

            // If not at the end of a statement, do not pop the stack past VeryLow priority ops.

            if stack_top.is_bin() && (end_statement || stack_top.priority() > Priority::VeryLow) {
                let expr = Rc::clone(&self.current_expr);
                self.current_expr = self.expr_stack.pop().unwrap();

                if !expr.is_empty() {
                    self.add_expr(&expr)?;
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn add_current_expr_to_group(&mut self) -> EvalResult {
        if self.current_expr.is_for() {
            if !self.current_expr.is_complete() {
                return Ok(()); // Wait for the FOR body
            }
        }

        // Handle the use case of erasing variables, e.g. $VAR = ;
        if self.current_expr.is_empty() {
            if let Some(top) = self.expr_stack.last() {
                if top.is_assignment() {
                    self.current_expr = self.expr_stack.pop().unwrap();
                }
            }
        }
        let group = Rc::clone(&self.group);

        if let Expression::Args(g) = &*group {
            self.pop_binary_ops(true)?;
            if !self.current_expr.is_empty() {
                g.borrow_mut().add_child(&self.current_expr)?;
            }
            self.pop_group()?;
        } else if !self.current_expr.is_empty() {
            if let Expression::Group(g) = &*group {
                self.pop_binary_ops(true)?;
                g.borrow_mut().add_child(&self.current_expr)?;
            } else {
                panic!("Unexpected group error");
            }
        }
        Ok(())
    }

    fn clear_current(&mut self) {
        self.current_expr = self.empty();
    }

    fn finalize_groups(&mut self) -> EvalResult {
        if self.group.is_args() {
            self.add_current_expr_to_group()?;

            if self.group.is_args() && !self.current_expr.is_cmd() {
                return error(
                    &*self.current_expr,
                    "Missing parentheses around FOR expression",
                );
            }
        }
        self.add_current_expr_to_group()
    }

    fn push(&mut self, group: Group) -> EvalResult {
        if group != Group::None {
            // Save the current scope
            let current_scope = Rc::clone(&self.scope);
            self.scope_stack.push(current_scope.clone());
            // Create new scope and make it current
            self.scope = Scope::new(Some(current_scope));
            // Start a new group
            self.group_stack.push(Rc::clone(&self.group));

            if group == Group::Args {
                self.group = new_args(self.prev_loc, &self.scope);
                self.prev_loc = self.loc;
            } else {
                self.group = new_group(self.prev_loc, &self.scope);
                self.prev_loc = self.loc;
            }
        }
        self.expr_stack.push(Rc::clone(&self.current_expr));
        self.clear_current();

        Ok(())
    }

    fn pop(&mut self) -> EvalResult {
        self.finalize_groups()?;
        self.pop_group()
    }

    fn pop_group(&mut self) -> EvalResult {
        if self.group_stack.is_empty() {
            return Err(EvalError::new(
                self.loc,
                "Unbalanced parentheses?".to_string(),
            ));
        }

        Self::close_group(&self.group);
        let group = Rc::clone(&self.group);

        self.group = self.group_stack.pop().unwrap(); // Restore group
        self.scope = self.scope_stack.pop().unwrap(); // Restore scope

        // Add the group itself to the expression previously saved on the stack
        if !self.expr_stack.is_empty() {
            self.current_expr = self.expr_stack.pop().unwrap();
            self.add_expr(&group)?;
        }

        Ok(())
    }

    fn parse(&mut self, quit: &mut bool) -> EvalResult<Rc<Expression>> {
        loop {
            let tok = self.next_token()?;
            match &tok {
                Token::End => {
                    break;
                }
                Token::LeftParen => {
                    self.push(Group::Block)?;
                }
                Token::RightParen => {
                    if self.group_stack.is_empty() {
                        return error(self, "Unmatched right parenthesis");
                    }
                    self.pop()?;
                }
                Token::Semicolon => {
                    self.finalize_groups()?;

                    // Semicolons end both statements and FOR argument lists;
                    // Do not clear the current expression in the latter case,
                    // since the expression is still being parsed (the body is
                    // expected after the argument list).
                    if !self.current_expr.is_for() {
                        self.clear_current();
                    }
                }
                Token::Keyword(word) => {
                    if word == "QUIT" {
                        *quit = true;
                        break;
                    }
                    if word == "IF" {
                        let expr = Rc::new(Expression::Branch(RefCell::new(BranchExpr {
                            cond: self.empty(),
                            if_branch: self.empty(),
                            else_branch: self.empty(),
                            expect_else: false, // becomes true once "else" keyword is seen
                            loc: self.prev_loc,
                            scope: Rc::clone(&self.scope),
                        })));
                        self.add_expr(&expr)?;
                    } else if word == "IN" {
                        if let Expression::For(f) = &*self.current_expr {
                            if f.borrow().var.is_empty() {
                                return error(self, "Expecting identifier in FOR expression");
                            }
                            self.prev_loc = self.loc;
                        } else {
                            return error(self, "IN without FOR");
                        }
                        self.push(Group::Args)?; // args will be added to ForExpr when finalized
                    } else if word == "ELSE" {
                        if let Expression::Branch(b) = &*self.current_expr {
                            if !b.borrow_mut().is_else_expected() {
                                return error(self, "Conditional expression or IF branch missing");
                            }
                            self.prev_loc = self.loc;
                            self.expect_else_expr = true;
                            self.push(Group::None)?;
                        } else {
                            return error(self, "ELSE without IF");
                        }
                    } else if word == "FOR" {
                        let expr = Rc::new(Expression::For(RefCell::new(ForExpr {
                            var: String::default(),
                            args: self.empty(),
                            body: self.empty(),
                            loc: self.prev_loc,
                            scope: Rc::clone(&self.scope),
                        })));
                        self.add_expr(&expr)?;
                        self.current_expr = expr;
                    } else if word == "WHILE" {
                        let expr = Rc::new(Expression::Loop(RefCell::new(LoopExpr {
                            cond: self.empty(),
                            body: self.empty(),
                            loc: self.prev_loc,
                            scope: Rc::clone(&self.scope),
                        })));
                        self.add_expr(&expr)?;
                    } else if word == "BREAK" || word == "CONTINUE" {
                        let expr = Rc::new(Expression::Leaf(Rc::new(Literal {
                            tok: word.clone(),
                            quoted: false,
                            loc: self.prev_loc,
                            scope: Rc::clone(&self.scope),
                        })));
                        self.add_expr(&expr)?;
                    }
                }
                Token::Literal((s, quoted)) => {
                    if !quoted && !self.group.is_args() {
                        if let Some(cmd) = get_command(s) {
                            let expr = Rc::new(Expression::Cmd(RefCell::new(Command {
                                cmd,
                                args: self.empty(),
                                loc: self.prev_loc,
                                scope: Rc::clone(&self.scope),
                            })));
                            self.add_expr(&expr)?;

                            self.current_expr = expr;
                            self.push(Group::Args)?; // args will be added to command when finalized

                            continue;
                        }
                    }
                    // Identifiers and literals. TODO: handle variables (identifiers) separately.
                    let expr = Rc::new(Expression::Leaf(Rc::new(Literal {
                        tok: s.clone(),
                        quoted: *quoted,
                        loc: self.prev_loc,
                        scope: Rc::clone(&self.scope),
                    })));
                    if !self.current_expr.is_empty() || !self.rewrite_pipeline(&expr)? {
                        self.add_expr(&expr)?;
                    }
                }
                Token::Operator(op) => {
                    let is_low_priority = op.priority() <= Priority::Low;

                    if is_low_priority {
                        if self.group.is_args() {
                            // Finish the arguments of the left hand-side expression
                            self.add_current_expr_to_group()?;
                        }
                        self.pop_binary_ops(false)?;
                    }

                    let expr = Rc::new(Expression::Bin(RefCell::new(BinExpr {
                        op: op.clone(),
                        lhs: Rc::clone(&self.current_expr),
                        rhs: self.empty(),
                        loc: self.prev_loc,
                        scope: Rc::clone(&self.scope),
                    })));

                    self.prev_loc = self.loc;

                    if is_low_priority {
                        self.expr_stack.push(Rc::clone(&expr));
                        self.clear_current();
                    } else {
                        self.current_expr = expr;
                    }
                }
            }
        }

        self.finalize_parse()
    }

    fn finalize_parse(&mut self) -> EvalResult<Rc<Expression>> {
        self.finalize_groups()?;

        if !self.expr_stack.is_empty() {
            let msg = if self.expect_else_expr {
                "Dangling ELSE"
            } else {
                my_dbg!(&self.expr_stack);
                "Missing closed parenthesis or expression operand"
            };
            return error(self, msg);
        }
        assert!(self.group_stack.is_empty()); // because the expr_stack is empty

        Self::close_group(&self.group);
        Ok(Rc::clone(&self.group))
    }

    fn rewrite_pipeline(&mut self, expr: &Rc<Expression>) -> EvalResult<bool> {
        assert!(self.current_expr.is_empty());

        let mut head = self.empty();
        let mut tail = self.empty();

        while let Some(top) = self.expr_stack.last().cloned() {
            if top.is_pipe() {
                //println!("pipe: {}", &top);
                if !head.is_empty() {
                    self.current_expr = Rc::clone(&top);
                    self.add_expr(&head)?;
                    //println!("current: {}", &self.current_expr);
                }
                if tail.is_empty() {
                    if let Expression::Bin(b) = &*top {
                        assert!(b.borrow().op == Op::Pipe);
                        tail = Rc::clone(&b.borrow().lhs);
                        head = Rc::clone(&tail);
                    }
                    //println!("end: {}", &tail);
                } else {
                    head = Rc::clone(&top);
                }
                self.expr_stack.pop();
            } else {
                //println!("top: {}", &top);
                break;
            }
        }
        //println!("start: [{}], end: [{}]", &head, &tail);
        if head.is_empty() {
            Ok(false)
        } else {
            self.current_expr = Rc::new(Expression::Bin(RefCell::new(BinExpr {
                op: Op::Pipe,
                lhs: Rc::clone(&head),
                rhs: Rc::clone(&expr),
                loc: expr.loc(),
                scope: Rc::clone(&self.scope),
            })));
            //println!("current_expr: {}", &self.current_expr);
            Ok(true)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    val: Rc<RefCell<Value>>,
}

impl Variable {
    fn new(val: Value) -> Self {
        Self {
            val: Rc::new(RefCell::new(val)),
        }
    }

    fn assign(&self, val: Value) {
        *self.val.borrow_mut() = val;
    }

    pub fn value(&self) -> Value {
        self.val.borrow().clone()
    }
}

impl From<&str> for Variable {
    fn from(value: &str) -> Self {
        Variable {
            val: Rc::new(RefCell::new(value.parse::<Value>().unwrap())),
        }
    }
}

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.val.borrow())
    }
}

#[derive(PartialEq)]
pub struct Scope {
    pub parent: Option<Rc<Scope>>,
    pub vars: RefCell<HashMap<String, Variable>>,
}

impl Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parent_addr: Option<String> = self.parent.as_ref().map(|p| format!("{:p}", p));

        f.debug_struct("Scope")
            .field("address", &format_args!("{:p}", self))
            .field("parent", &parent_addr)
            .finish()
    }
}

impl Scope {
    fn new(parent: Option<Rc<Scope>>) -> Rc<Scope> {
        Rc::new(Self {
            parent: parent,
            vars: RefCell::new(HashMap::new()),
        })
    }

    fn with_env_vars() -> Rc<Scope> {
        let mut vars = env::vars()
            .map(|(key, value)| (key, Variable::from(value.as_str())))
            .collect::<HashMap<_, _>>();

        let shell = env::current_exe().unwrap_or(PathBuf::from("mysh"));
        vars.insert(
            "SHELL".to_string(),
            Variable::from(shell.to_string_lossy().to_string().as_str()),
        );

        Rc::new(Scope {
            parent: None,
            vars: RefCell::new(vars),
        })
    }

    pub fn is_interrupted(&self) -> bool {
        crate::INTERRUPT.load(SeqCst)
    }

    pub fn insert(&self, var_name: String, val: Value) {
        self.vars.borrow_mut().insert(var_name, Variable::new(val));
    }

    pub fn lookup(&self, var_name: &str) -> Option<Variable> {
        match self.vars.borrow().get(var_name) {
            Some(v) => Some(v.clone()),
            None => match &self.parent {
                Some(scope) => scope.lookup(var_name),
                _ => None,
            },
        }
    }

    pub fn lookup_local(&self, var_name: &str) -> Option<Variable> {
        self.vars.borrow().get(var_name).cloned()
    }

    pub fn lookup_partial(&self, var_name: &str) -> Vec<String> {
        let mut keys = Vec::new();

        for key in self.vars.borrow().keys() {
            if key.to_lowercase().starts_with(var_name) {
                keys.push(key.clone())
            }
        }
        keys
    }

    pub fn lookup_value(&self, var_name: &str) -> Option<Value> {
        match self.lookup(var_name) {
            Some(v) => Some(v.value()),
            None => None,
        }
    }

    fn clear(&self) {
        self.vars.borrow_mut().clear();
    }

    /// Lookup and erase a variable
    pub fn erase(&self, var_name: &str) -> Option<Variable> {
        match self.vars.borrow_mut().remove(var_name) {
            Some(var) => Some(var),
            None => match &self.parent {
                Some(scope) => scope.lookup(var_name),
                _ => None,
            },
        }
    }
}

/// Parses and expands shell-like variable expressions in a given string.
/// # Note
/// Groups need to be enclosed in quotes, to distinguish from normal parentheses.
/// Captures need to be double escaped.
///
/// # Examples
///
/// Assuming the following variables are in scope:
/// - `NAME="John Doe"`
/// - `GREETING="Hello, World!"`
///
/// Basic variable expansion:
/// ```
/// "${NAME}"         -> "John Doe"
/// "$GREETING"       -> "Hello, World!"
/// ```
///
/// Variable substitution:
/// ```
/// "${NAME/John/Jane}"            -> "Jane Doe"
/// "${GREETING/World/Universe}"   -> "Hello, Universe!"
/// ```
///
/// Capture groups in substitution:
/// ```
/// "${NAME/(\\w+) (\\w+)/\\2, \\1}"   -> "Doe, John"
/// "${GREETING/(Hello), (World)!/\\2 says \\1}" -> "World says Hello"
/// ```
///
/// Handling non-existent variables:
/// ```
/// "${UNDEFINED_VAR}"             -> ""
/// "${UNDEFINED_VAR/foo/bar}"     -> ""
/// ```
fn parse_value(s: &str, loc: Location, scope: &Rc<Scope>) -> EvalResult<Value> {
    let re = Regex::new(r"\$\{([^}]+)\}|\$([a-zA-Z_][a-zA-Z0-9_]*)")
        .map_err(|e| EvalError::new(loc, e.to_string()))?;

    let result = re.replace_all(s, |caps: &regex::Captures| {
        let var_expr = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str())
            .unwrap_or("");

        let parts: Vec<&str> = var_expr.splitn(3, '/').collect();
        let var_name = parts[0];

        match scope.lookup(var_name) {
            Some(var) => {
                let mut value = var.value().to_string();

                if parts.len() == 3 {
                    let search = parts[1];
                    // Recursively expand variables in the replacement pattern.
                    let replace = parse_value(parts[2], loc, scope)
                        .unwrap_or(Value::default())
                        .to_string();

                    if let Ok(re) = Regex::new(search) {
                        // Implement bash-like substitution with capture groups
                        value = re
                            .replace_all(&value, |caps: &regex::Captures| {
                                let mut result = replace.to_string();
                                for (i, cap) in caps.iter().enumerate().skip(1) {
                                    if let Some(m) = cap {
                                        result = result.replace(&format!("\\{}", i), m.as_str());
                                    }
                                }
                                result
                            })
                            .into_owned();
                    }
                }

                value
            }
            None => String::default(), // Return empty string if VAR not found
        }
    });

    result
        .parse::<Value>()
        .map_err(|e| EvalError::new(loc, e.to_string()))
}

#[derive(Debug)]
enum Expression {
    Empty,
    Args(RefCell<GroupExpr>),
    Bin(RefCell<BinExpr>),
    Cmd(RefCell<Command>),
    Branch(RefCell<BranchExpr>),
    For(RefCell<ForExpr>),
    Group(RefCell<GroupExpr>),
    Leaf(Rc<Literal>), // Values and identifiers
    Loop(RefCell<LoopExpr>),
}

impl Expression {
    fn is_args(&self) -> bool {
        matches!(self, Expression::Args(_))
    }

    fn is_no_args(&self) -> bool {
        if let Expression::Args(g) = self {
            return g.borrow().content.is_empty();
        }
        false
    }

    fn is_assignment(&self) -> bool {
        if let Expression::Bin(bin_expr) = &self {
            return bin_expr.borrow().op == Op::Assign;
        }
        false
    }

    fn is_bin(&self) -> bool {
        matches!(self, Expression::Bin(_))
    }

    fn is_cmd(&self) -> bool {
        matches!(self, Expression::Cmd(_))
    }

    fn is_for(&self) -> bool {
        matches!(self, Expression::For(_))
    }

    fn is_empty(&self) -> bool {
        matches!(self, Expression::Empty)
    }

    fn is_group(&self) -> bool {
        matches!(self, Expression::Group(_))
    }

    fn is_number(&self) -> bool {
        if self.is_empty() {
            return false;
        }
        match self.eval() {
            Ok(Value::Int(_)) | Ok(Value::Real(_)) => true,
            _ => false,
        }
    }

    fn is_pipe(&self) -> bool {
        if let Expression::Bin(b) = self {
            b.borrow().op == Op::Pipe
        } else {
            false
        }
    }
    /// Is the expression completely constructed (parsed)?
    fn is_complete(&self) -> bool {
        match self {
            Expression::Args(group) => group.borrow().closed,
            Expression::Bin(bin_expr) => !&bin_expr.borrow().rhs.is_empty(),
            Expression::Cmd(cmd) => !&cmd.borrow().args.is_empty(),
            Expression::Branch(branch) => {
                let b = branch.borrow();
                if b.expect_else && b.else_branch.is_empty() {
                    return false;
                }
                !&b.if_branch.is_empty()
            }
            Expression::Group(group) => group.borrow().closed,
            Expression::For(for_expr) => !&for_expr.borrow().body.is_empty(),
            Expression::Empty => false,
            Expression::Leaf(_) => true,
            Expression::Loop(loop_expr) => !&loop_expr.borrow().body.is_empty(),
        }
    }

    /// Argument evaluation
    fn to_values(&self) -> EvalResult<Vec<Value>> {
        match &self {
            Expression::Args(args) => {
                let mut values = Vec::new();

                for v in args.borrow().iter() {
                    values.push(Status::check_result(v)?);
                }
                Ok(values)
            }
            _ => error(self, "Expecting argument list"),
        }
    }

    /// Evaluate and tokenize arguments
    fn tokenize_args(&self) -> EvalResult<Vec<String>> {
        let mut tokens = Vec::new();

        for val in &self.to_values()? {
            tokens.extend(val.to_string().split_ascii_whitespace().map(String::from));
        }

        // Read from stdin if args consist of a single dash
        if tokens.len() == 1 && tokens[0] == "-" {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|e| EvalError::new(self.loc(), e.to_string()))?;
            tokens = buffer.split_ascii_whitespace().map(String::from).collect();
        }

        Ok(tokens)
    }

    fn priority(&self) -> Priority {
        match self {
            Expression::Args(_) => Priority::High,
            Expression::Bin(bin_expr) => bin_expr.borrow().op.priority(),
            Expression::Cmd(_) => Priority::High,
            Expression::Branch(_) => Priority::High,
            Expression::Group(_) => Priority::High,
            Expression::For(_) => Priority::High,
            Expression::Empty => Priority::High,
            Expression::Leaf(_) => Priority::High,
            Expression::Loop(_) => Priority::High,
        }
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expression::Args(group) => write!(f, "{}", group.borrow()),
            Expression::Bin(bin_expr) => write!(f, "{}", bin_expr.borrow()),
            Expression::Cmd(cmd) => write!(f, "{}", cmd.borrow()),
            Expression::Branch(branch) => write!(f, "{}", branch.borrow()),
            Expression::Group(group) => write!(f, "{}", group.borrow()),
            Expression::For(for_expr) => write!(f, "{}", for_expr.borrow()),
            Expression::Empty => write!(f, ""),
            Expression::Leaf(literal) => write!(f, "{}", literal),
            Expression::Loop(loop_expr) => write!(f, "{}", loop_expr.borrow()),
        }
    }
}

impl HasLocation for Expression {
    fn loc(&self) -> Location {
        match self {
            Expression::Args(group) => group.borrow().loc(),
            Expression::Bin(bin_expr) => bin_expr.borrow().loc(),
            Expression::Cmd(cmd) => cmd.borrow().loc(),
            Expression::Branch(branch) => branch.borrow().loc(),
            Expression::Group(group) => group.borrow().loc(),
            Expression::For(for_expr) => for_expr.borrow().loc(),
            Expression::Empty => panic!("Empty expression"),
            Expression::Leaf(literal) => literal.loc(),
            Expression::Loop(loop_expr) => loop_expr.borrow().loc(),
        }
    }
}

#[derive(Debug)]
struct BinExpr {
    op: Op,
    lhs: Rc<Expression>,
    rhs: Rc<Expression>,
    loc: Location,
    scope: Rc<Scope>, // Scope needed for assignment op.
}

derive_has_location!(BinExpr);

impl ExprNode for BinExpr {
    /// Add right hand-side child expression.
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        if self.rhs.is_empty() {
            self.rhs = Rc::clone(child);
            Ok(())
        } else {
            error(&**child, "Unexpected expression")
        }
    }
}

impl fmt::Display for BinExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.lhs, self.op, self.rhs)
    }
}

/// Division evaluator helper
macro_rules! div_match {
    ($self:expr, $i:expr, $rhs:expr) => {
        match $rhs {
            Value::Int(j) => {
                if j == 0 {
                    error($self, "Division by zero")
                } else {
                    Ok(Value::Real(($i as f64) / (j as f64)))
                }
            }
            Value::Real(j) => {
                if j == 0.0 {
                    error($self, "Division by zero")
                } else {
                    Ok(Value::Real(($i as f64) / j))
                }
            }
            Value::Str(s) => Ok(Value::Str(format!("{}/{}", $i, s))),
            Value::Stat(_) => error($self, "Cannot divide by command status"),
        }
    };
}

/// Macro that generates comparison functions
macro_rules! eval_cmp_fn {
    ($fn_name:ident, $op:tt) => {
        fn $fn_name(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
            match self.eval_cmp(lhs, rhs)? {
                Value::Real(r) => Ok(Value::Int((r $op 0.0) as i64)),
                _ => panic!("Unexpected result type in comparison"),
            }
        }
    }
}

fn get_own_path() -> Result<String, String> {
    match env::current_exe() {
        Ok(p) => {
            #[cfg(test)]
            {
                let path_str = p.to_string_lossy();
                #[cfg(windows)]
                {
                    let re = Regex::new(r"\\deps\\.*?(\..*)?$").map_err(|e| e.to_string())?;
                    Ok(re.replace(&path_str, "\\mysh$1").to_string())
                }
                #[cfg(not(windows))]
                {
                    let re = Regex::new(r"/deps/.+?(\..*)?$").map_err(|e| e.to_string())?;
                    Ok(re.replace(&path_str, "/mysh$1").to_string())
                }
            }
            #[cfg(not(test))]
            {
                Ok(p.to_string_lossy().to_string())
            }
        }
        Err(e) => Err(format!("Failed to get executable name: {}", e)),
    }
}

impl BinExpr {
    fn eval_and(&self) -> EvalResult<Value> {
        let lhs_val = self.lhs.eval()?;
        if let Value::Stat(s) = &lhs_val {
            if s.borrow().result.is_err() {
                return Ok(lhs_val); // Return unchecked Status
            }
        }

        let rhs_val = self.rhs.eval()?;
        if let Value::Stat(s) = &rhs_val {
            if s.borrow().result.is_err() {
                return Ok(rhs_val); // Return unchecked Status
            }
        }
        let all = value_as_bool(&lhs_val, &self.scope) && value_as_bool(&rhs_val, &self.scope);

        Ok(Value::Int(all as _))
    }

    fn eval_or(&self) -> EvalResult<Value> {
        let lhs_val = self.lhs.eval()?;
        let mut any = value_as_bool(&lhs_val, &self.scope);

        if !any {
            let rhs_val = self.rhs.eval()?;
            if let Value::Stat(_) = &rhs_val {
                return Ok(rhs_val); // Return delayed Status
            }
            any = value_as_bool(&rhs_val, &self.scope);
        }

        Ok(Value::Int(any as _))
    }

    fn eval_assign(&self, rhs_in: Value) -> EvalResult<Value> {
        // Does the right hand-side value wrap a command Status?
        let rhs = if let Value::Stat(status) = &rhs_in {
            // Assign the command execution result (success or fail?) to a variable
            Value::Int(status.borrow_mut().as_bool(&self.scope) as _)
        } else {
            rhs_in
        };

        if let Expression::Leaf(lit) = &*self.lhs {
            let var_name = &lit.tok;

            if var_name.starts_with('$') {
                // Assigning to an already-defined variable, as in: $i = $i + 1?
                if let Some(var) = lit.scope.lookup(&var_name[1..]) {
                    var.assign(rhs);
                    return Ok(var.value());
                } else {
                    return error(self, &format!("Variable not found: {}", var_name));
                }
            } else {
                // Create new variable in the current scope
                self.scope.insert(var_name.to_owned(), rhs.clone());
                return Ok(rhs);
            }
        }
        error(self, "Identifier expected on left hand-side of assignment")
    }

    fn eval_cmp_status(&self) -> EvalResult<Value> {
        let message = if self.op == Op::Gt {
            "Command status does not support '>', did you mean redirect '=>' ?"
        } else {
            "Command status can only be checked as true or false, not compared to other values"
        };
        error(self, message)
    }

    fn eval_cmp(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
        match lhs {
            Value::Int(i) => match rhs {
                Value::Int(j) => Ok(Value::Real((i - j) as _)),
                Value::Real(j) => Ok(Value::Real(i as f64 - j)),
                Value::Str(_) => error(self, "Cannot compare number to string"),
                Value::Stat(_) => self.eval_cmp_status(),
            },
            Value::Real(i) => match rhs {
                Value::Int(j) => Ok(Value::Real(i - j as f64)),
                Value::Real(j) => Ok(Value::Real(i - j)),
                Value::Str(_) => error(self, "Cannot compare number to string"),
                Value::Stat(_) => self.eval_cmp_status(),
            },
            Value::Str(s1) => match rhs {
                Value::Int(_) | Value::Real(_) => error(self, "Cannot compare string to number"),
                Value::Str(s2) => {
                    let ord = match s1.cmp(&s2) {
                        Ordering::Equal => 0,
                        Ordering::Less => -1,
                        Ordering::Greater => 1,
                    };
                    Ok(Value::Real(ord as _))
                }
                Value::Stat(_) => self.eval_cmp_status(),
            },
            Value::Stat(_) => self.eval_cmp_status(),
        }
    }

    eval_cmp_fn!(eval_equals, ==);
    eval_cmp_fn!(eval_not_equals, !=);
    eval_cmp_fn!(eval_lt, <);
    eval_cmp_fn!(eval_lte, <=);
    eval_cmp_fn!(eval_gt, >);
    eval_cmp_fn!(eval_gte, >=);

    fn eval_div(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
        match lhs {
            Value::Int(i) => div_match!(self, i, rhs),
            Value::Real(i) => div_match!(self, i, rhs),
            Value::Str(s1) => match rhs {
                Value::Int(_) | Value::Real(_) => Ok(Value::Str(format!("{}/{}", s1, rhs))),
                Value::Str(s2) => Ok(Value::Str(format!("{}/{}", s1, s2))),
                Value::Stat(_) => error(self, "Cannot divide by command status"),
            },
            Value::Stat(_) => error(self, "Cannot divide command status"),
        }
    }

    fn eval_int_div(&self, _lhs: Value, _rhs: Value) -> EvalResult<Value> {
        Err(EvalError::new(
            self.loc,
            "Integer division not implemented".to_string(),
        ))
    }

    fn eval_minus(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
        match lhs {
            Value::Int(i) => match rhs {
                Value::Int(j) => Ok(Value::Int(i - j)),
                Value::Real(j) => Ok(Value::Real(i as f64 - j)),
                Value::Str(_) => error(self, "Cannot subtract string from number"),
                Value::Stat(_) => error(self, "Cannot subtract command status from number"),
            },
            Value::Real(i) => match rhs {
                Value::Int(j) => Ok(Value::Real(i - j as f64)),
                Value::Real(j) => Ok(Value::Real(i - j)),
                Value::Str(_) => error(self, "Cannot subtract string from number"),
                Value::Stat(_) => error(self, "Cannot subtract command status from number"),
            },
            Value::Str(_) => match rhs {
                Value::Int(_) | Value::Real(_) => error(self, "Cannot subtract number from string"),
                Value::Str(_) => error(self, "Cannot subtract strings"),
                Value::Stat(_) => error(self, "Cannot subtract command status from string"),
            },
            Value::Stat(_) => error(self, "Cannot subtract command statuses"),
        }
    }

    fn eval_mod(&self, _lhs: Value, _rhs: Value) -> EvalResult<Value> {
        Err(EvalError::new(
            self.loc,
            "Modulo operation not implemented".to_string(),
        ))
    }

    fn eval_mul(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
        match lhs {
            Value::Int(i) => match rhs {
                Value::Int(j) => Ok(Value::Int(i * j)),
                Value::Real(j) => Ok(Value::Real(i as f64 * j)),
                Value::Str(_) => error(self, "Cannot multiply number by string"),
                Value::Stat(_) => error(self, "Cannot multiply number by command status"),
            },
            Value::Real(i) => match rhs {
                Value::Int(j) => Ok(Value::Real(i * j as f64)),
                Value::Real(j) => Ok(Value::Real(i * j)),
                Value::Str(_) => error(self, "Cannot multiply number by string"),
                Value::Stat(_) => error(self, "Cannot multiply number by command status"),
            },
            Value::Str(_) => match rhs {
                Value::Int(_) | Value::Real(_) => error(self, "Cannot multiply string by number"),
                Value::Str(_) => error(self, "Cannot multiply strings"),
                Value::Stat(_) => error(self, "Cannot multiply string by command status"),
            },
            Value::Stat(_) => error(self, "Cannot multiply command statuses"),
        }
    }

    /// Evaluate expr and redirect output into a String
    fn eval_redirect(&self, expr: &Rc<Expression>) -> EvalResult<String> {
        let mut redirect =
            BufferRedirect::stdout().map_err(|e| EvalError::new(self.loc, e.to_string()))?;

        Status::check_result(expr.eval())?;

        let mut str_buf = String::new();
        redirect
            .read_to_string(&mut str_buf)
            .map_err(|e| EvalError::new(self.loc, e.to_string()))?;

        Ok(str_buf.to_string())
    }

    fn eval_exit_code(&self, cmd: String, status: &std::process::ExitStatus) -> EvalResult<Value> {
        let exit_code = status.code().unwrap_or_else(|| -1);
        my_dbg!(exit_code);

        let result = if exit_code == 0 {
            Ok(Value::success())
        } else {
            Err(EvalError::new(
                self.loc(),
                format!("{}: exited with code {}", cmd, exit_code),
            ))
        };

        Ok(Value::Stat(Status::new(cmd, &result, &self.scope)))
    }

    fn eval_pipe_to_var(
        &self,
        lhs: &Rc<Expression>,
        rhs: &Rc<Expression>,
    ) -> EvalResult<Option<Value>> {
        // Piping into a literal? assign standard output capture to string variable.
        if let Expression::Leaf(lit) = &**rhs {
            // Special case: is the left hand-side expression a pipeline?
            let output = if lhs.is_pipe() {
                let program = get_own_path().map_err(|e| EvalError::new(self.loc, e))?;

                // Get the left hand-side expression as a string
                let lhs_str = lhs.to_string();

                // Start an instance of the interpreter to evaluate the left hand-side of the pipe
                // println!("Executing pipe LHS: {} -c {}", &program, &lhs_str);

                let mut child = StdCommand::new(&program)
                    .arg("-c")
                    .arg(&lhs_str)
                    .stdout(Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        EvalError::new(rhs.loc(), format!("Failed to spawn child process: {}", e))
                    })?;

                let mut buffer = Vec::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_end(&mut buffer).map_err(|e| {
                        EvalError::new(rhs.loc(), format!("Failed to read output: {}", e))
                    })?;
                }

                // Wait for the child process to complete
                let exit_status = child.wait().map_err(|e| {
                    EvalError::new(
                        rhs.loc(),
                        format!("Failed to wait for child process output: {}", e),
                    )
                })?;

                self.eval_exit_code(lhs_str, &exit_status)?;

                String::from_utf8(buffer).map_err(|e| {
                    EvalError::new(
                        rhs.loc(),
                        format!("Failed to convert pipe output from UTF8: {}", e),
                    )
                })?
            } else {
                // Base use case, left hand-side is not a pipe expression
                self.eval_redirect(lhs)?
            };
            let value = Value::from_str(output.trim())?;
            self.scope.insert(lit.tok.clone(), value.clone());

            return Ok(Some(value));
        }
        Ok(None)
    }

    fn eval_pipe(&self, lhs: &Rc<Expression>, rhs: &Rc<Expression>) -> EvalResult<Value> {
        if lhs.is_empty() {
            return error(self, "Expecting pipe input");
        }

        if let Some(val) = self.eval_pipe_to_var(lhs, rhs)? {
            return Ok(val);
        }

        // Create a pipe
        let (reader, writer) = match os_pipe::pipe() {
            Ok((r, w)) => (r, w),
            Err(e) => return error(self, &format!("Failed to create pipe: {}", e)),
        };

        // Redirect stdout to the pipe
        let redirect = match Redirect::stdout(writer) {
            Ok(r) => r,
            Err(e) => return error(self, &format!("Failed to redirect stdout: {}", e)),
        };

        // Get our own program name
        let program = get_own_path().map_err(|e| EvalError::new(self.loc, e))?;

        // Get the right-hand side expression as a string
        let rhs_str = rhs.to_string();

        // Start a copy of the running program with the arguments "-c" rhs_str
        // to evaluate the right hand-side of the pipe expression

        // println!("Executing pipe RHS: {} -c {}", &program, &rhs_str);

        let child = StdCommand::new(&program)
            .arg("-c")
            .arg(&rhs_str)
            .stdin(Stdio::from(reader))
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| {
                EvalError::new(rhs.loc(), format!("Failed to spawn child process: {}", e))
            })?;

        // Left-side evaluation's stdout goes into the pipe.
        let lhs_result = Status::check_result(lhs.eval());

        // Drop the redirect to close the write end of the pipe
        drop(redirect);

        // Wait for the child process to complete and get its output
        let output = child.wait_with_output().map_err(|e| {
            EvalError::new(
                rhs.loc(),
                format!("Failed to get child process output: {}", e),
            )
        })?;

        lhs_result?; // Check for any left hand-side errors

        // Print the output of the right hand-side expression.
        print!("{}", String::from_utf8_lossy(&output.stdout));

        self.eval_exit_code(rhs_str, &output.status)
    }

    /// Binary plus
    fn eval_plus(&self, lhs: Value, rhs: Value) -> EvalResult<Value> {
        match lhs {
            Value::Int(i) => match rhs {
                Value::Int(j) => Ok(Value::Int(i + j)),
                Value::Real(j) => Ok(Value::Real(i as f64 + j)),
                Value::Str(ref s) => Ok(Value::Str(format!("{}{}", i, s))),
                Value::Stat(_) => error(self, "Cannot add number and command status"),
            },
            Value::Real(i) => match rhs {
                Value::Int(j) => Ok(Value::Real(i + j as f64)),
                Value::Real(j) => Ok(Value::Real(i + j)),
                Value::Str(ref s) => Ok(Value::Str(format!("{}{}", i, s))),
                Value::Stat(_) => error(self, "Cannot add number and command status"),
            },
            Value::Str(s) => Ok(Value::Str(format!("{}{}", s, rhs))),
            Value::Stat(_) => error(self, "Cannot add command statuses"),
        }
    }

    /// Lookup and erase the variable named by the left hand-side expression
    fn eval_erase(&self) -> EvalResult<Value> {
        if let Expression::Leaf(lit) = &*self.lhs {
            let var_name = &lit.tok;

            if var_name.starts_with('$') {
                if let Some(var) = lit.scope.erase(&var_name[1..]) {
                    return Ok(var.value()); // Return the erased value
                } else {
                    return error(self, &format!("Variable not found: {}", var_name));
                }
            }
        }
        error(self, "Variable expected on left hand-side of assignment")
    }

    /// Redirect standard output to file
    fn eval_write(&self, append: bool) -> EvalResult<Value> {
        let filename = self.rhs.eval()?.to_string();
        let operation = if append { "append" } else { "overwrite" };
        if Path::new(&filename).exists()
            && confirm(
                format!("{} exists, confirm {}", filename, operation),
                &self.scope,
                false,
            )
            .map_err(|e| EvalError::new(self.loc, e.to_string()))?
                != Answer::Yes
        {
            return Ok(Value::Int(401));
        }

        // Open destination file
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(&filename)
            .map_err(|e| EvalError::new(self.loc, e.to_string()))?;

        // Redirect stdout to the file
        let _redirect = Redirect::stdout(file)
            .map_err(|e| EvalError::new(self.loc, format!("Failed to redirect stdout: {}", e)))?;

        // Evaluate left hand-side expression
        self.lhs.eval()
    }
}

macro_rules! eval_bin {
    ($self:expr, $f:ident) => {
        $self.$f($self.lhs.eval()?, $self.rhs.eval()?)
    };
}

impl Eval for BinExpr {
    fn eval(&self) -> EvalResult<Value> {
        if self.rhs.is_empty() {
            if self.op == Op::Assign {
                return self.eval_erase(); // Assign empty, erase variable
            }
            error(self, "Expecting right hand-side operand")
        } else if self.lhs.is_empty() {
            if self.op.is_unary_ok() {
                eval_unary(self, &self.op, self.rhs.eval()?, &self.scope)
            } else {
                error(self, "Expecting left hand-side operand")
            }
        } else {
            match self.op {
                Op::And => self.eval_and(),
                Op::Append => self.eval_write(true),
                Op::Assign => self.eval_assign(self.rhs.eval()?.clone()),
                Op::Div => eval_bin!(self, eval_div),
                Op::Gt => eval_bin!(self, eval_gt),
                Op::Gte => eval_bin!(self, eval_gte),
                Op::IntDiv => eval_bin!(self, eval_int_div),
                Op::Equals => eval_bin!(self, eval_equals),
                Op::Lt => eval_bin!(self, eval_lt),
                Op::Lte => eval_bin!(self, eval_lte),
                Op::Minus => eval_bin!(self, eval_minus),
                Op::Mod => eval_bin!(self, eval_mod),
                Op::Mul => eval_bin!(self, eval_mul),
                Op::Not => error(self, "Unexpected logical negation operator"),
                Op::NotEquals => eval_bin!(self, eval_not_equals),
                Op::Or => self.eval_or(),
                Op::Pipe => self.eval_pipe(&self.lhs, &self.rhs),
                Op::Plus => eval_bin!(self, eval_plus),
                Op::Write => self.eval_write(false),
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Group {
    None,
    Args,
    Block,
}

#[derive(Debug)]
struct GroupExpr {
    kind: Group,
    closed: bool,
    scope: Rc<Scope>,
    content: Vec<Rc<Expression>>,
    loc: Location,
}

impl GroupExpr {
    fn new_args(loc: Location, scope: &Rc<Scope>) -> Self {
        Self {
            kind: Group::Args,
            scope: Rc::clone(&scope),
            content: Vec::new(),
            loc,
            closed: false,
        }
    }

    fn new_group(loc: Location, scope: &Rc<Scope>) -> Self {
        Self {
            kind: Group::Block,
            content: Vec::new(),
            loc,
            scope: Rc::clone(&scope),
            closed: false,
        }
    }

    fn iter(&self) -> impl Iterator<Item = EvalResult<Value>> + '_ {
        self.content.iter().map(|expr| expr.eval())
    }
}

derive_has_location!(GroupExpr);

impl Eval for GroupExpr {
    fn eval(&self) -> EvalResult<Value> {
        self.scope.clear();

        let mut result = Ok(Value::success());

        for e in &self.content {
            // Check the previous result for unhandled command errors
            result = Status::check_result(result);

            if result.is_ok() {
                let temp = e.eval();

                if let Ok(Value::Str(word)) = &temp {
                    if word == "BREAK" {
                        result = Err(EvalError {
                            loc: e.loc(),
                            message: "BREAK outside loop".to_string(),
                            jump: Some(Jump::Break(result.unwrap())),
                        });
                        break;
                    } else if word == "CONTINUE" {
                        result = Err(EvalError {
                            loc: e.loc(),
                            message: "CONTINUE outside loop".to_string(),
                            jump: Some(Jump::Continue(result.unwrap())),
                        });
                        break;
                    }
                }
                result = temp;
            }
        }

        result // Return the last evaluation
    }
}

impl ExprNode for GroupExpr {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        self.content.push(Rc::clone(child));
        Ok(())
    }
}

fn join_expr(expressions: &[Rc<Expression>], separator: &str) -> String {
    expressions
        .iter()
        .map(|expr| expr.to_string())
        .collect::<Vec<_>>()
        .join(separator)
}

impl fmt::Display for GroupExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind == Group::Args {
            write!(f, "{}", join_expr(&self.content, " "))
        } else {
            write!(f, "( {} )", join_expr(&self.content, "; "))
        }
    }
}

#[derive(Debug)]
struct Command {
    cmd: ShellCommand,
    args: Rc<Expression>,
    loc: Location,
    scope: Rc<Scope>,
}

derive_has_location!(Command);

macro_rules! define_redir_enum {
    ($name:ident, $other_stream:ident) => {
        enum $name {
            #[allow(dead_code)]
            File(Redirect<File>),
            #[allow(dead_code)]
            $other_stream(Redirect<io::$other_stream>),
            #[allow(dead_code)]
            Null(Gag),
            None,
        }
    };
}

macro_rules! define_redir_impl {
    ($name:ident, $self_stream:ident, $other_stream:ident, $other:ident, $lookup_key:expr, $other_key:expr) => {
        impl $name {
            fn with_scope(scope: &Rc<Scope>) -> Result<Self, String> {
                if let Some(v) = scope.lookup_value($lookup_key) {
                    let path = v.to_string();
                    if path == $other_key || path == concat!("__", $other_key) {
                        return Ok($name::$other_stream(
                            Redirect::$self_stream(io::$other()).map_err(|e| {
                                format!(
                                    "Failed to redirect {} to {}: {}",
                                    $lookup_key, $other_key, e
                                )
                            })?,
                        ));
                    }
                    if path.to_ascii_lowercase() == "null" {
                        return Ok($name::Null(Gag::$self_stream().map_err(|e| e.to_string())?));
                    }
                    let file = OpenOptions::new()
                        .truncate(true)
                        .read(true)
                        .create(true)
                        .write(true)
                        .open(&path)
                        .map_err(|e| {
                            format!(
                                "Failed to open file for {} redirection '{}': {}",
                                $lookup_key, path, e
                            )
                        })?;

                    return Ok($name::File(Redirect::$self_stream(file).map_err(|e| {
                        format!(
                            "Failed to redirect {} to file '{}': {}",
                            $lookup_key, path, e
                        )
                    })?));
                }

                Ok($name::None)
            }
        }
    };
}

macro_rules! handle_redir_error {
    ($redir:expr, $loc:expr) => {
        if let Err(message) = &$redir {
            if !message.contains("Redirect already exists") {
                return Err(EvalError::new($loc, message.clone()));
            }
        }
    };
}

// Define RedirStderr
define_redir_enum!(RedirStderr, Stdout);
define_redir_impl!(RedirStderr, stderr, Stdout, stdout, "__stderr", "1");

// Define RedirStdout
define_redir_enum!(RedirStdout, Stderr);
define_redir_impl!(RedirStdout, stdout, Stderr, stderr, "__stdout", "2");

impl Eval for Command {
    fn eval(&self) -> EvalResult<Value> {
        // Redirect stdout if a $__stdout variable found in scope.
        // Values can be "2", "__stderr", "null", or a filename.
        let redir_stdout = RedirStdout::with_scope(&self.scope);
        handle_redir_error!(&redir_stdout, self.loc);

        // Redirect stderr if a $__stderr variable found in scope.
        // Values can be "1", "__stdout", "null", or a filename.
        let redir_stderr = RedirStderr::with_scope(&self.scope);
        handle_redir_error!(&redir_stderr, self.loc);

        // Evaluate command line arguments and convert to strings
        let args = self
            .args
            .to_values()?
            .into_iter()
            .map(|v| v.to_string())
            .collect();
        // Execute command
        let result = self
            .cmd
            .exec(&self.cmd.name(), &args, &self.scope)
            .map_err(|e| EvalError::new(self.args.loc(), e));

        let cmd = self.to_string();
        Ok(Value::Stat(Status::new(cmd, &result, &self.scope)))
    }
}

impl ExprNode for Command {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        assert!(child.is_args());
        assert!(self.args.is_empty());
        self.args = Rc::clone(&child);
        Ok(())
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.args.is_no_args() {
            return write!(f, "{}", self.cmd.name());
        }
        write!(f, "{} {}", self.cmd.name(), self.args)
    }
}

#[derive(Debug)]
struct BranchExpr {
    cond: Rc<Expression>,
    if_branch: Rc<Expression>,
    else_branch: Rc<Expression>,
    expect_else: bool,
    loc: Location,
    scope: Rc<Scope>,
}

derive_has_location!(BranchExpr);

impl BranchExpr {
    fn is_else_expected(&mut self) -> bool {
        if !self.cond.is_empty() && !self.if_branch.is_empty() {
            self.expect_else = true;
            return true;
        }
        false
    }
}

fn hoist(scope: &Rc<Scope>, var_name: &str) {
    if let Some(v) = scope.lookup_local(var_name) {
        if let Some(parent) = &scope.parent {
            // topmost scope is for environment vars
            if parent.parent.is_some() {
                parent.insert(var_name.to_string(), v.value());
            }
        }
    }
}

fn value_as_bool(val: &Value, scope: &Rc<Scope>) -> bool {
    let result = match val {
        Value::Int(i) => *i != 0,
        Value::Real(r) => *r != 0.0,
        Value::Str(s) => !s.is_empty(), // TODO: maybe not such a good idea?
        Value::Stat(s) => s.borrow_mut().as_bool(&scope),
    };

    hoist(scope, "__errors");

    result
}

fn eval_as_bool(expr: &Rc<Expression>, scope: &Rc<Scope>) -> EvalResult<bool> {
    Ok(value_as_bool(&expr.eval()?, &scope))
}

impl ExprNode for BranchExpr {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        if self.cond.is_empty() {
            self.cond = Rc::clone(child);
        } else if self.if_branch.is_empty() {
            if !child.is_group() {
                return error(&**child, "Parentheses are required around IF block");
            }
            self.if_branch = Rc::clone(child);
        } else if self.else_branch.is_empty() {
            if !self.expect_else {
                return error(&**child, "Expecting ELSE keyword");
            }
            if !child.is_group() {
                return error(&**child, "Parentheses are required around ELSE block");
            }
            self.else_branch = Rc::clone(child);
        } else {
            return error(&**child, "Unexpected expression after ELSE block");
        }
        Ok(())
    }
}

impl Eval for BranchExpr {
    fn eval(&self) -> EvalResult<Value> {
        if self.cond.is_empty() {
            return error(self, "Expecting IF condition");
        } else if self.if_branch.is_empty() {
            return error(self, "Expecting IF block");
        }
        if eval_as_bool(&self.cond, &self.scope)? {
            self.if_branch.eval()
        } else if self.else_branch.is_empty() {
            Ok(Value::success())
        } else {
            self.else_branch.eval()
        }
    }
}

impl fmt::Display for BranchExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "if {} {}", self.cond, self.if_branch)?;

        if !self.else_branch.is_empty() {
            write!(f, " else {}", self.else_branch)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Literal {
    tok: String,
    quoted: bool,
    loc: Location,
    scope: Rc<Scope>,
}

derive_has_location!(Literal);

impl Eval for Literal {
    fn eval(&self) -> EvalResult<Value> {
        parse_value(&self.tok, self.loc, &self.scope)
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.quoted {
            write!(f, "\"{}\"", &self.tok)
        } else {
            write!(f, "{}", &self.tok)
        }
    }
}

#[derive(Debug)]
struct LoopExpr {
    cond: Rc<Expression>,
    body: Rc<Expression>,
    loc: Location,
    scope: Rc<Scope>,
}

derive_has_location!(LoopExpr);

macro_rules! eval_iteration {
    ($self:expr, $result:ident) => {
        if $self.scope.is_interrupted() {
            eprintln!("^C");
            break; // Bail on Ctrl+C
        }

        // Evaluate the loop body, checking for command status
        $result = Status::check_result($self.body.eval());

        // Check for break, continue
        if let Err(e) = &$result {
            match &e.jump {
                Some(Jump::Break(v)) => {
                    $result = Ok(v.clone());
                    break;
                }
                Some(Jump::Continue(v)) => {
                    $result = Ok(v.clone());
                }
                None => {
                    break;
                }
            }
        }
    };
}

impl Eval for LoopExpr {
    fn eval(&self) -> EvalResult<Value> {
        if self.cond.is_empty() {
            return error(self, "Expecting WHILE condition");
        } else if self.body.is_empty() {
            return error(self, "Expecting WHILE body");
        }
        let mut result = Ok(Value::success());
        loop {
            if !eval_as_bool(&self.cond, &self.scope)? {
                break;
            }
            eval_iteration!(self, result);
        }
        result
    }
}

impl ExprNode for LoopExpr {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        if self.cond.is_empty() {
            self.cond = Rc::clone(child);
        } else if self.body.is_empty() {
            if !child.is_group() {
                return error(&**child, "Parentheses are required around WHILE body");
            }
            self.body = Rc::clone(&child);
        } else {
            return error(&**child, "WHILE already has a body");
        }
        Ok(())
    }
}

impl fmt::Display for LoopExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "while {} {}", self.cond, self.body)
    }
}

#[derive(Debug)]
struct ForExpr {
    var: String,
    args: Rc<Expression>,
    body: Rc<Expression>,
    loc: Location,
    scope: Rc<Scope>,
}

derive_has_location!(ForExpr);

impl Eval for ForExpr {
    fn eval(&self) -> EvalResult<Value> {
        if self.var.is_empty() {
            return error(self, "Expecting FOR variable");
        }
        if self.args.is_empty() || self.args.is_no_args() {
            return error(self, "Expecting FOR arguments");
        }
        if self.body.is_empty() {
            return error(self, "Expecting FOR body");
        }

        let mut result = Ok(Value::success());

        let args = self.args.tokenize_args()?;
        for arg in &args {
            self.scope.insert(self.var.clone(), arg.parse::<Value>()?);
            eval_iteration!(self, result);
        }

        result
    }
}

impl ExprNode for ForExpr {
    fn add_child(&mut self, child: &Rc<Expression>) -> EvalResult {
        if self.var.is_empty() {
            if let Expression::Leaf(lit) = &**child {
                self.var = lit.tok.clone();
                return Ok(());
            }
            return error(self, "Expecting identifier FOR expression");
        } else if self.args.is_empty() {
            if child.is_args() {
                self.args = Rc::clone(&child);
            } else {
                return error(self, "Expecting argument list");
            }
        } else if self.body.is_empty() {
            if !child.is_group() {
                return error(&**child, "Parentheses are required around FOR body");
            }
            self.body = Rc::clone(&child);
        } else {
            return error(self, "FOR already has a body");
        }
        Ok(())
    }
}

impl fmt::Display for ForExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "for {} in {}; {}", &self.var, self.args, self.body)
    }
}

fn eval_unary<T: HasLocation>(
    loc: &T,
    op: &Op,
    val: Value,
    scope: &Rc<Scope>,
) -> EvalResult<Value> {
    match op {
        Op::Minus => match val {
            Value::Int(i) => Ok(Value::Int(-i)),
            Value::Real(r) => Ok(Value::Real(-r)),
            Value::Str(s) => Ok(Value::Str(format!("-{}", s))),
            Value::Stat(_) => error(loc, "Unary minus not supported for command status"),
        },
        Op::Not => {
            if let Value::Stat(s) = &val {
                hoist(&scope, "__errors");
                s.borrow_mut().negated = true;
                Ok(val)
            } else {
                Ok(Value::Int(!value_as_bool(&val, &scope) as _))
            }
        }
        _ => error(loc, "Unexpected unary operation"),
    }
}

impl Eval for Expression {
    fn eval(&self) -> EvalResult<Value> {
        match &self {
            Expression::Args(g) => g.borrow().eval(),
            Expression::Bin(b) => b.borrow().eval(),
            Expression::Branch(b) => b.borrow().eval(),
            Expression::Cmd(c) => c.borrow().eval(),
            Expression::Group(g) => g.borrow().eval(),
            Expression::For(f) => f.borrow().eval(),
            Expression::Empty => {
                panic!("Empty expression");
            }
            Expression::Leaf(lit) => lit.eval(),
            Expression::Loop(l) => l.borrow().eval(),
        }
    }
}

pub struct Interp {
    scope: Rc<Scope>,
}

fn new_args(loc: Location, scope: &Rc<Scope>) -> Rc<Expression> {
    Rc::new(Expression::Args(RefCell::new(GroupExpr::new_args(
        loc, &scope,
    ))))
}

fn new_group(loc: Location, scope: &Rc<Scope>) -> Rc<Expression> {
    Rc::new(Expression::Group(RefCell::new(GroupExpr::new_group(
        loc, &scope,
    ))))
}

impl Interp {
    pub fn new() -> Self {
        Self {
            scope: Scope::with_env_vars(),
        }
    }

    pub fn eval(&self, quit: &mut bool, input: &str) -> EvalResult<Value> {
        let ast = self.parse(quit, input)?;

        if self.scope.lookup("DUMP_AST").is_some() {
            dbg!(&ast);
        }

        Status::check_result(ast.eval())
    }

    fn parse(&self, quit: &mut bool, input: &str) -> EvalResult<Rc<Expression>> {
        let mut parser = Parser::new(input.chars(), &self.scope);
        parser.parse(quit)
    }

    pub fn set_var(&mut self, name: &str, value: String) {
        self.scope.insert(name.to_string(), Value::Str(value))
    }

    pub fn get_scope(&self) -> Rc<Scope> {
        Rc::clone(&self.scope)
    }
}
