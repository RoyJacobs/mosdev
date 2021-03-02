#![allow(dead_code)]

use std::fmt::Display;
use std::io::Write;
use std::ops::Deref;

use clap::{App, Arg, ArgMatches};
use fs_err::{read_to_string, OpenOptions};
use itertools::Itertools;
use serde::Deserialize;

use crate::config::Config;
use crate::core::parser::{
    parse_or_err, AddressingMode, ArgItem, Expression, ExpressionFactor, Located, ParseTree, Token,
    Trivia,
};
use crate::errors::MosResult;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Casing {
    Uppercase,
    Lowercase,
}

impl Casing {
    fn format(&self, s: &str) -> String {
        match self {
            Casing::Uppercase => s.to_uppercase(),
            Casing::Lowercase => s.to_lowercase(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct MnemonicOptions {
    pub one_per_line: bool,
    pub casing: Casing,
    pub register_casing: Casing,
}

impl Default for MnemonicOptions {
    fn default() -> Self {
        Self {
            one_per_line: true,
            casing: Casing::Lowercase,
            register_casing: Casing::Lowercase,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum BracePosition {
    SameLine,
    NewLine,
    AsIs,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct BraceOptions {
    pub position: BracePosition,
    pub double_newline_after_closing_brace: bool,
}

impl Default for BraceOptions {
    fn default() -> Self {
        Self {
            position: BracePosition::SameLine,
            double_newline_after_closing_brace: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WhitespaceOptions {
    pub trim: bool,
    pub indent: usize,
    pub space_between_kvp: bool,
    pub space_between_expression_factors: bool,
    pub space_before_end_of_line_comments: bool,
    pub collapse_multiple_empty_lines: bool,
    pub newline_before_if: bool,
    pub newline_before_variables: bool,
    pub newline_before_pc: bool,
    pub newline_before_label: bool,
}

impl Default for WhitespaceOptions {
    fn default() -> Self {
        Self {
            trim: true,
            indent: 4,
            space_between_kvp: true,
            space_between_expression_factors: true,
            space_before_end_of_line_comments: true,
            collapse_multiple_empty_lines: true,
            newline_before_if: true,
            newline_before_variables: true,
            newline_before_pc: true,
            newline_before_label: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FormattingOptions {
    pub mnemonics: MnemonicOptions,
    pub braces: BraceOptions,
    pub whitespace: WhitespaceOptions,
}

impl FormattingOptions {
    fn passthrough() -> Self {
        Self {
            mnemonics: MnemonicOptions {
                one_per_line: false,
                casing: Casing::Lowercase,
                register_casing: Casing::Lowercase,
            },
            braces: BraceOptions {
                position: BracePosition::AsIs,
                double_newline_after_closing_brace: false,
            },
            whitespace: WhitespaceOptions {
                trim: false,
                indent: 0,
                space_between_kvp: false,
                space_between_expression_factors: false,
                space_before_end_of_line_comments: false,
                collapse_multiple_empty_lines: false,
                newline_before_if: false,
                newline_before_variables: false,
                newline_before_pc: false,
                newline_before_label: false,
            },
        }
    }

    fn default_no_indent() -> Self {
        let mut options = FormattingOptions::default();
        options.whitespace.indent = 0;
        options
    }
}

impl Default for FormattingOptions {
    fn default() -> Self {
        Self {
            mnemonics: MnemonicOptions::default(),
            braces: BraceOptions::default(),
            whitespace: WhitespaceOptions::default(),
        }
    }
}

struct CodeFormatter<'a> {
    ast: &'a [Token],
    options: FormattingOptions,
    index: usize,
    indent: usize,
    output: Vec<(usize, String)>,
}

impl<'a> CodeFormatter<'a> {
    fn new(ast: &'a [Token], options: FormattingOptions) -> Self {
        Self {
            ast,
            options,
            index: 0,
            indent: 0,
            output: vec![],
        }
    }

    fn nested(&self, ast: &'a [Token], options: FormattingOptions) -> Self {
        Self {
            ast,
            options,
            index: 0,
            indent: self.indent + self.options.whitespace.indent,
            output: vec![],
        }
    }

    fn parent_token(&self) -> Option<&Token> {
        self.ast.get(self.index)
    }

    fn prev_non_trivia_token(&self) -> Option<&Token> {
        let mut idx = self.index;
        loop {
            if idx == 0 {
                return None;
            }

            idx -= 1;
            let token = self.ast.get(idx).unwrap_or_else(|| {
                panic!("idx: {} // len: {}", idx, self.ast.len());
            });
            match &token {
                Token::EolTrivia(_) | Token::Eof(_) => (),
                _ => {
                    return Some(&token);
                }
            }
        }
    }

    fn format(&mut self) -> String {
        self.index = 0;
        for item in self.ast {
            let str = self.format_token(item);

            if !str.is_empty() {
                // Determine the indent level for this token.
                // If it was EolTrivia we don't want to indent, since it gets tacked on to the end of the previous token
                // which is already indented.
                let indent_level = match &item {
                    Token::EolTrivia(_) => 0,
                    _ => self.indent,
                };
                self.output.push((indent_level, str));
            }
            self.index += 1;
        }

        indent_str(&self.output)
    }

    fn with_options<F: FnOnce(&mut Self) -> T, T>(&mut self, f: F) -> T {
        let old = self.options;
        let result = f(self);
        self.options = old;
        result
    }

    fn preceding_newline_len(&self) -> usize {
        let items: Vec<&str> = self.output.iter().map(|(_, s)| s.as_str()).collect();
        preceding_newline_len(&items)
    }

    fn format_located_token<'f: 'a>(&mut self, lt: &'f Located<Token>) -> String {
        format!("{}{}", self.format_trivia(lt), self.format_token(&lt.data))
    }

    fn format_token<'f: 'a>(&mut self, token: &'f Token) -> String {
        match &token {
            Token::Align { tag, value } => {
                let ws = self.options.whitespace.space_between_kvp;
                let tag = self.format_located(tag);
                let value = space_prefix_if(
                    format!(
                        "{}{}",
                        self.format_trivia(value),
                        self.format_expression(&value.data)
                    ),
                    ws,
                );
                format!("{}{}", tag, value)
            }
            Token::Braces {
                lparen,
                inner,
                rparen,
            }
            | Token::Config {
                lparen,
                inner,
                rparen,
            } => {
                let inner = self.nested(inner, self.options).format();

                let inner = if self.options.whitespace.trim {
                    // When we trim, we need to re-indent afterwards
                    let inner = vec![(
                        self.indent + self.options.whitespace.indent,
                        inner.trim().to_string(),
                    )];
                    indent_str(&inner)
                } else {
                    inner
                };

                // Right paren needs a newline only if the inner tokens don't already end with a newline
                let rparen_newline_prefix: String = if inner.ends_with('\n') || inner.is_empty() {
                    "".into()
                } else {
                    "\n".into()
                };

                let (lparen, rparen) = self.with_options(|f| {
                    f.options.whitespace.trim = true;

                    match f.options.braces.position {
                        BracePosition::SameLine => {
                            // Left paren needs a newline or a space depending on parent token
                            let lparen_newline_prefix: String = match f.parent_token() {
                                Some(Token::If { .. })
                                | Some(Token::Label { .. })
                                | Some(Token::Segment { .. })
                                | Some(Token::Definition { .. }) => " ".into(),
                                _ => "\n".into(),
                            };

                            let lparen = format!(
                                "{}{}{{{}",
                                lparen_newline_prefix,
                                space_prefix(f.format_trivia(&lparen).trim_start().to_string()),
                                "\n",
                            );
                            let rparen =
                                format!("{}{}}}", rparen_newline_prefix, f.format_trivia(&rparen));

                            (lparen, rparen)
                        }
                        BracePosition::NewLine => {
                            let lparen = format!(
                                "\n{}{{\n",
                                f.format_trivia(&lparen).trim_start().to_string(),
                            );
                            let rparen =
                                format!("{}{}}}", rparen_newline_prefix, f.format_trivia(&rparen));

                            (lparen, rparen)
                        }
                        BracePosition::AsIs => {
                            let lparen = f.format_display(&lparen);
                            let rparen = f.format_display(&rparen);
                            (lparen, rparen)
                        }
                    }
                });

                let rparen = if self.options.braces.double_newline_after_closing_brace
                    && self.options.braces.position != BracePosition::AsIs
                {
                    format!("{}\n\n", rparen)
                } else {
                    rparen
                };

                format!("{}{}{}", lparen, inner, rparen)
            }
            Token::ConfigPair { key, eq, value } => {
                let key = self.format_located_token(key);
                let ws = self.options.whitespace.space_between_kvp;
                let eq = space_prefix_if(self.format_located(eq), ws);
                let value = space_prefix_if(self.format_located_token(value), ws);
                format!("{}{}{}", key, eq, value)
            }
            Token::Data { values, size } => {
                let ws = self.options.whitespace.space_between_kvp;
                let size = self.format_located(size);
                let values = space_prefix_if(self.format_args(values), ws);
                format!("{}{}", size, values)
            }
            Token::Definition { tag, id, value } => {
                let value = self.format_optional_token(value);
                format!("{}{}{}", tag, id, value)
            }
            Token::EolTrivia(eol) | Token::Eof(eol) => {
                let preceding_newline_len = self.preceding_newline_len();

                if self.options.whitespace.collapse_multiple_empty_lines
                    && preceding_newline_len > 0
                {
                    // Do nothing
                    "".into()
                } else {
                    let eol = eol.map_once(|_| "\n");
                    let eol = self.format_located(&eol);

                    // If the trivia isn't just newlines, add a space (i.e. "nop // hello" instead of "nop//hello")
                    if !eol.trim().is_empty()
                        && self.options.whitespace.space_before_end_of_line_comments
                    {
                        format!(" {}", eol)
                    } else {
                        eol
                    }
                }
            }
            Token::Error(_) => panic!("Should not be formatting ASTs that contain errors"),
            Token::Expression(expr) => self.format_expression(expr),
            Token::IdentifierName(id) => id.0.to_string(),
            Token::If {
                tag_if,
                value,
                if_,
                tag_else,
                else_,
            } => {
                let ws = self.options.whitespace.space_between_kvp;
                let tag_if = self.format_located(tag_if);
                let value = space_prefix_if(
                    format!(
                        "{}{}",
                        self.format_trivia(value),
                        self.format_expression(&value.data)
                    ),
                    ws,
                );
                let if_ = self.with_options(|f| {
                    // No newline after the 'if' block, if there is an else block
                    if else_.is_some() {
                        f.options.braces.double_newline_after_closing_brace = false;
                    }
                    f.format_token(if_)
                });
                let tag_else = space_prefix_if(self.format_optional_located(tag_else), ws);
                let else_ = self.format_optional_token(else_);
                let result = format!("{}{}{}{}{}", tag_if, value, if_, tag_else, else_);

                if self.options.whitespace.newline_before_if && self.preceding_newline_len() <= 1 {
                    format!("\n{}", result)
                } else {
                    result
                }
            }
            Token::Include {
                tag,
                lquote,
                filename,
            } => {
                let tag = self.format_located(tag);
                let lquote = space_prefix(self.format_located(lquote));
                let filename = self.format_located(filename);
                format!("{}{}{}\"", tag, lquote, filename)
            }
            Token::Instruction(i) => {
                let mnemonic_data = self
                    .options
                    .mnemonics
                    .casing
                    .format(&i.mnemonic.data.to_string());

                let mnemonic = self.format_located(&i.mnemonic.map_once(|_| mnemonic_data));

                // Was the previous token an instruction?
                let mnemonic = match self.prev_non_trivia_token() {
                    Some(Token::Instruction(_)) => {
                        // Yes. Let's make sure the instructions are split correctly.
                        if self.options.mnemonics.one_per_line {
                            // We need to add a line ending if there were no previous newlines
                            if self.preceding_newline_len() == 0 {
                                format!("\n{}", mnemonic.trim_start())
                            } else {
                                // Had newlines, so we don't need to add a newline
                                mnemonic.trim_start().into()
                            }
                        } else if self.format_trivia(&i.mnemonic).is_empty() {
                            format!(" {}", mnemonic)
                        } else {
                            mnemonic
                        }
                    }
                    _ => {
                        // No. So let's trim any preceding whitespace, if requested.
                        if self.options.whitespace.trim {
                            mnemonic.trim_start().into()
                        } else {
                            mnemonic
                        }
                    }
                };

                let operand = match &i.operand {
                    Some(o) => {
                        let lchar = self.format_optional_located(&o.lchar);
                        let rchar = self.format_optional_located(&o.rchar);
                        let expr = format!(
                            "{}{}",
                            self.format_trivia(&o.expr),
                            self.format_expression(&o.expr.data)
                        );

                        let suffix = match &o.suffix {
                            Some(suffix) => {
                                let comma = self.format_located(&suffix.comma);
                                let register = suffix.register.map(|r| {
                                    self.options
                                        .mnemonics
                                        .register_casing
                                        .format(&format!("{}", r))
                                });
                                let register = self.format_located(&register);
                                format!("{}{}", comma, register)
                            }
                            None => "".to_string(),
                        };

                        match &o.addressing_mode {
                            AddressingMode::AbsoluteOrZP => {
                                format!("{}{}", expr, suffix)
                            }
                            AddressingMode::Immediate => {
                                format!("{}{}", lchar, expr)
                            }
                            AddressingMode::Implied => "".to_string(),
                            AddressingMode::OuterIndirect => {
                                format!("{}{}{}{}", lchar, expr, rchar, suffix)
                            }
                            AddressingMode::Indirect => {
                                format!("{}{}{}{}", lchar, expr, suffix, rchar)
                            }
                        }
                    }
                    None => "".to_string(),
                };

                let operand = space_prefix(operand);

                format!("{}{}", mnemonic, operand)
            }
            Token::Label { id, colon, braces } => {
                let id = self.format_located(id);
                let colon = self.format_located(colon);
                let braces = self.format_optional_token(braces);
                let result = format!("{}{}{}", id, colon, braces);

                // If the previous instruction was not a label definition, add a newline
                if self.options.whitespace.newline_before_label
                    && !matches!(self.prev_non_trivia_token(), Some(Token::Label { .. }))
                {
                    format!("\n{}", result)
                } else {
                    result
                }
            }
            Token::ProgramCounterDefinition { star, eq, value } => {
                let ws = self.options.whitespace.space_between_kvp;
                let star = self.format_located(star);
                let eq = space_prefix_if(self.format_located(eq), ws);
                let value = space_prefix_if(
                    format!(
                        "{}{}",
                        self.format_trivia(value),
                        self.format_expression(&value.data)
                    ),
                    ws,
                );
                let result = format!("{}{}{}", star, eq, value);

                // If the previous instruction was not a PC definition, add a newline
                if self.options.whitespace.newline_before_pc
                    && !matches!(
                        self.prev_non_trivia_token(),
                        Some(Token::ProgramCounterDefinition { .. })
                    )
                {
                    format!("\n{}", result)
                } else {
                    result
                }
            }
            Token::Segment { tag, id, inner } => {
                let ws = self.options.whitespace.space_between_kvp;
                let tag = self.format_located(tag);
                let id = space_prefix_if(self.format_located_token(id), ws);
                let inner = self.format_optional_token(inner);
                format!("{}{}{}", tag, id, inner)
            }
            Token::VariableDefinition { ty, id, eq, value } => {
                let ws = self.options.whitespace.space_between_kvp;
                let ty = self.format_located(ty);
                let id = space_prefix_if(self.format_located(id), ws);
                let eq = space_prefix_if(self.format_located(eq), ws);
                let value = space_prefix_if(
                    format!(
                        "{}{}",
                        self.format_trivia(&value),
                        self.format_expression(&value.data)
                    ),
                    ws,
                );
                let result = format!("{}{}{}{}", ty, id, eq, value);

                // If the previous instruction was not a variable definition, add a newline
                if self.options.whitespace.newline_before_variables
                    && !matches!(
                        self.prev_non_trivia_token(),
                        Some(Token::VariableDefinition { .. })
                    )
                {
                    format!("\n{}", result)
                } else {
                    result
                }
            }
        }
    }

    fn format_located<T: Display>(&self, lt: &Located<T>) -> String {
        format!("{}{}", self.format_trivia(lt), lt.data)
    }

    fn format_optional_located<T: Display>(&self, lt: &Option<Located<T>>) -> String {
        lt.as_ref()
            .map(|lt| format!("{}{}", self.format_trivia(&lt), lt.data))
            .unwrap_or_else(|| "".to_string())
    }

    fn format_optional_token<T: Deref<Target = Token>>(&mut self, lt: &'a Option<T>) -> String {
        lt.as_ref()
            .map(|t| self.format_token(t.deref()))
            .unwrap_or_else(|| "".to_string())
    }

    fn format_optional_located_token<T: Deref<Target = Located<Token>>>(
        &mut self,
        lt: &'a Option<T>,
    ) -> String {
        lt.as_ref()
            .map(|t| self.format_located_token(t.deref()))
            .unwrap_or_else(|| "".to_string())
    }

    fn format_trivia<T>(&self, lt: &Located<T>) -> String {
        match &lt.trivia {
            Some(t) => {
                if self.options.whitespace.trim {
                    let mut last_trivia_was_space = true;
                    let mut result: String = "".into();
                    for item in &t.data {
                        let triv = match item {
                            Trivia::Whitespace(_) => {
                                if last_trivia_was_space {
                                    "".into()
                                } else {
                                    last_trivia_was_space = true;
                                    " ".into()
                                }
                            }
                            Trivia::NewLine => "".into(),
                            _ => {
                                if last_trivia_was_space {
                                    last_trivia_was_space = false;
                                    format!("{}", item)
                                } else {
                                    // Leave some whitespace between comments
                                    format!(" {}", item)
                                }
                            }
                        };
                        result = format!("{}{}", result, triv);
                    }

                    result
                } else {
                    // Just concat the trivia
                    t.data.iter().map(|item| format!("{}", item)).join("")
                }
            }
            None => "".to_string(),
        }
    }

    fn format_display<T: Display>(&mut self, display: &T) -> String {
        format!("{}", display)
    }

    fn format_optional_display<T: Display>(&mut self, display: &Option<T>) -> String {
        display
            .as_ref()
            .map(|d| format!("{}", d))
            .unwrap_or_else(|| "".to_string())
    }

    fn format_expression(&mut self, expr: &'a Expression) -> String {
        match expr {
            Expression::BinaryExpression(be) => {
                let op = self.format_located(&be.op);
                let lhs = format!(
                    "{}{}",
                    self.format_trivia(&be.lhs),
                    self.format_expression(&be.lhs.data)
                );
                let rhs = format!(
                    "{}{}",
                    self.format_trivia(&be.rhs),
                    self.format_expression(&be.rhs.data)
                );
                let ws = self.options.whitespace.space_between_expression_factors;
                format!(
                    "{}{}{}",
                    lhs,
                    space_prefix_if(op, ws),
                    space_prefix_if(rhs, ws)
                )
            }
            Expression::Factor {
                factor,
                tag_not,
                tag_neg,
                ..
            } => {
                let tag_not = self.format_optional_display(tag_not);
                let tag_neg = self.format_optional_display(tag_neg);
                let factor = format!(
                    "{}{}",
                    self.format_trivia(factor),
                    self.format_expression_factor(&factor.data)
                );
                format!("{}{}{}", tag_not, tag_neg, factor)
            }
        }
    }

    fn format_expression_factor(&mut self, factor: &'a ExpressionFactor) -> String {
        match factor {
            ExpressionFactor::CurrentProgramCounter(star) => self.format_located(star),
            ExpressionFactor::ExprParens {
                lparen,
                inner,
                rparen,
            } => {
                let lparen = self.format_located(lparen);
                let inner = format!(
                    "{}{}",
                    self.format_trivia(inner),
                    self.format_expression(&inner.data)
                );
                let rparen = self.format_located(rparen);
                format!("{}{}{}", lparen, inner, rparen)
            }
            ExpressionFactor::FunctionCall {
                name,
                lparen,
                args,
                rparen,
            } => {
                let name = self.format_located_token(name);
                let lparen = self.format_located(lparen);
                let args = self.format_args(args);
                let rparen = self.format_located(rparen);
                format!("{}{}{}{}", name, lparen, args, rparen)
            }
            ExpressionFactor::IdentifierValue { path, modifier } => {
                let path = self.format_located(path);
                let modifier = self.format_optional_display(modifier);
                format!("{}{}", modifier, path)
            }
            ExpressionFactor::Number { ty, value } => {
                let value = self.format_located(&value);
                let ty = self.format_located(ty);
                format!("{}{}", ty, value)
            }
        }
    }

    fn format_args(&self, args: &[ArgItem]) -> String {
        args.iter()
            .map(|(arg, comma)| {
                let arg = self.format_located(arg);
                let comma = self.format_optional_located(comma);
                format!("{}{}", arg, comma)
            })
            .join("")
    }
}

fn space_prefix<S: AsRef<str>>(val: S) -> String {
    let val = val.as_ref();
    if !val.trim().is_empty() && !val.starts_with(' ') {
        format!(" {}", val.trim())
    } else {
        val.into()
    }
}

fn space_prefix_if<S: AsRef<str>>(val: S, condition: bool) -> String {
    let val = val.as_ref();
    if condition {
        space_prefix(val)
    } else {
        val.into()
    }
}

fn contains_newline_trivia(trivia: &Option<Box<Located<Vec<Trivia>>>>) -> bool {
    match trivia {
        Some(trivia) => trivia
            .data
            .iter()
            .any(|item| matches!(item, Trivia::NewLine)),
        None => false,
    }
}

fn format(ast: &ParseTree, options: FormattingOptions) -> String {
    let mut fmt = CodeFormatter::new(ast.tokens(), options);
    let result = fmt.format();
    if options.whitespace.trim {
        result.trim().into()
    } else {
        result
    }
}

pub fn format_app() -> App<'static> {
    App::new("format").about("Formats input file(s)").arg(
        Arg::new("input")
            .about("Sets the input file(s) to use")
            .required(true)
            .multiple(true),
    )
}

pub fn format_command(args: &ArgMatches, cfg: Option<Config>) -> MosResult<()> {
    let formatting_options = cfg.map(|cfg| cfg.formatting).unwrap_or_default();

    let input_names = args.values_of("input").unwrap().collect_vec();

    for input_name in input_names {
        let source = read_to_string(input_name)?;
        let ast = parse_or_err(input_name.as_ref(), &source)?;
        let formatted = format(&ast, formatting_options.unwrap_or_default());
        let formatted = formatted.replace("\n", crate::LINE_ENDING);
        let mut output_file = OpenOptions::new()
            .truncate(true)
            .write(true)
            .open(input_name)?;
        output_file.write_all(formatted.as_bytes())?;
    }

    Ok(())
}

fn preceding_newline_len(lines: &[&str]) -> usize {
    let mut sum = 0;
    for line in lines.iter().rev() {
        let trimmed = line.trim_end_matches('\n');
        let endings = line.len() - trimmed.len();
        sum += endings;

        // We only want to continue if the previous line consisted entirely out of line endings
        if line.len() != endings {
            return sum;
        }
    }
    sum
}

fn generate_indent_prefix(indent: usize) -> String {
    let mut indent_prefix = "".to_string();
    for _ in 0..indent {
        indent_prefix += " "
    }
    indent_prefix
}

/// Indents multiple strings with their associated indent level, each string containing multiple lines.
fn indent_str(input: &[(usize, String)]) -> String {
    // We now have a vector containing indent levels and big strings containing multiple lines.
    input
        .iter()
        .map(|(indent, lines)| {
            // Now, per string, we're going to:
            // - Determine if there was at least 1 newline at the end
            //   This final newline is going to be removed during the join(), so we need to re-add it
            // - Indent all the lines and join them
            // - Re-add the final newline, if needed
            let ending_newline = preceding_newline_len(&[lines]) > 0;
            let indented = lines
                .lines()
                .map(|line| {
                    if line.trim().is_empty() {
                        // Don't try to indent lines consisting of trivia
                        line.to_string()
                    } else {
                        // Determine this line's indent level. If it is not idented enough, we will indent it.
                        let trimmed_line = line.trim_start_matches(' ');
                        let cur_indent = line.len() - trimmed_line.len();
                        if cur_indent < *indent {
                            format!("{}{}", generate_indent_prefix(*indent), trimmed_line)
                        } else {
                            line.to_string()
                        }
                    }
                })
                .join("\n");

            if ending_newline {
                indented + "\n"
            } else {
                indented
            }
        })
        .join("")
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;

    use crate::commands::*;
    use crate::core::parser::parse_or_err;
    use crate::errors::{MosError, MosResult};

    use super::{format, indent_str, preceding_newline_len, FormattingOptions};

    #[test]
    fn test_preceding_newline_len() {
        assert_eq!(preceding_newline_len(&["a"]), 0);
        assert_eq!(preceding_newline_len(&["a", "\n"]), 1);
        assert_eq!(preceding_newline_len(&["a", "\nb"]), 0);
        assert_eq!(preceding_newline_len(&["a", "\nb", "\n\n"]), 2);
        assert_eq!(preceding_newline_len(&["a", "\nb", "\n", "\n"]), 2);
    }

    #[test]
    fn test_indent_str() {
        eq(indent_str(&[(0, "a\n  b\n    c".into())]), "a\n  b\n    c");
        eq(indent_str(&[(1, "a\n  b\n    c".into())]), " a\n  b\n    c");
        eq(
            indent_str(&[(4, "a\n  b\n    c".into())]),
            "    a\n    b\n    c",
        );
    }

    #[test]
    fn default_formatting_splits_instructions() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), "lda #123 sta 123\nstx 123\nrol")?;
        eq(
            format(&ast, FormattingOptions::default()),
            "lda #123\nsta 123\nstx 123\nrol",
        );

        Ok(())
    }

    #[test]
    fn whitespace_trim() -> MosResult<()> {
        let ast = parse_or_err(
            "test.asm".as_ref(),
            "/* hello */    /* foo */  lda #123 rol",
        )?;
        let mut options = FormattingOptions::passthrough();

        options.whitespace.trim = true;
        eq(format(&ast, options), "/* hello */ /* foo */ lda #123 rol");

        Ok(())
    }

    #[test]
    fn braces() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), ".segment a {}nop")?;
        let mut options = FormattingOptions::passthrough();
        options.braces.double_newline_after_closing_brace = true;

        options.braces.position = BracePosition::SameLine;
        eq(format(&ast, options), ".segment a {\n}\n\nnop");

        options.braces.position = BracePosition::NewLine;
        eq(format(&ast, options), ".segment a\n{\n}\n\nnop");

        Ok(())
    }

    #[test]
    fn braces_after_pc() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), "* = $1000\n{ nop }")?;
        let mut options = FormattingOptions::passthrough();
        options.whitespace.trim = true;
        options.whitespace.space_between_kvp = true;

        options.braces.position = BracePosition::SameLine;
        eq(format(&ast, options), "* = $1000\n{\nnop\n}");

        options.braces.position = BracePosition::NewLine;
        eq(format(&ast, options), "* = $1000\n{\nnop\n}");

        Ok(())
    }

    #[test]
    fn braces_with_content() -> MosResult<()> {
        let ast = parse_or_err(
            "test.asm".as_ref(),
            ".segment a\n\n\n\n  {\n\nnop\n\n\n}nop",
        )?;

        let mut options = FormattingOptions::passthrough();
        options.braces.double_newline_after_closing_brace = true;
        options.braces.position = BracePosition::SameLine;
        eq(format(&ast, options), ".segment a {\n\n\nnop\n\n\n}\n\nnop");
        options.braces.position = BracePosition::NewLine;
        eq(
            format(&ast, options),
            ".segment a\n{\n\n\nnop\n\n\n}\n\nnop",
        );

        Ok(())
    }

    #[test]
    fn default_formatting_collapses_newlines_after_rparen() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), ".segment a {\nbrk\n}\n\n\n\nnop")?;
        eq(
            format(&ast, FormattingOptions::default_no_indent()),
            ".segment a {\nbrk\n}\n\nnop",
        );

        Ok(())
    }

    #[test]
    fn default_formatting_braces_indent() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), ".if foo {\nnop /* hi */\n}")?;
        eq(
            format(&ast, FormattingOptions::default()),
            ".if foo {\n    nop /* hi */\n}",
        );

        Ok(())
    }

    #[test]
    fn default_formatting_label_indent() -> MosResult<()> {
        let ast = parse_or_err("test.asm".as_ref(), "foo:\n\n{\nnop\nbrk\n\nrol}asl")?;
        eq(
            format(&ast, FormattingOptions::default()),
            "foo: {\n    nop\n    brk\n    rol\n}\n\nasl",
        );

        Ok(())
    }

    #[test]
    fn default_formatting_newline_before_if() -> MosResult<()> {
        let ast = parse_or_err(
            "test.asm".as_ref(),
            "nop\n.if test { nop }\n.if test { brk }\n\n\n\n\n.if test { rol }",
        )?;
        eq(
            format(&ast, FormattingOptions::default_no_indent()),
            "nop\n\n.if test {\nnop\n}\n\n.if test {\nbrk\n}\n\n.if test {\nrol\n}",
        );

        Ok(())
    }

    #[test]
    fn default_formatting_options() -> MosResult<()> {
        let source = include_str!("../../test/cli/format/valid-unformatted.asm");
        let expected = include_str!("../../test/cli/format/valid-formatted.asm");
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(&ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn passthrough_formatting_should_result_in_original() -> MosResult<()> {
        let source = include_str!("../../test/cli/format/valid-unformatted.asm");
        let expected = include_str!("../../test/cli/format/valid-unformatted.asm");
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(&ast, FormattingOptions::passthrough());

        eq(actual, expected);

        Ok(())
    }

    #[test]
    fn can_read_config() -> MosResult<()> {
        let toml = include_str!("../../test/cli/format/mos-default-formatting.toml");
        let cfg: FormattingOptions = toml::from_str(toml).map_err(MosError::from)?;
        assert_eq!(cfg, FormattingOptions::default());

        Ok(())
    }

    // Cross-platform eq
    fn eq<S: AsRef<str>, T: AsRef<str>>(actual: S, expected: T) {
        use crate::LINE_ENDING;

        // Split the result into lines to work around cross-platform line ending normalization issues
        assert_eq!(
            actual.as_ref().lines().join(LINE_ENDING),
            expected.as_ref().lines().join(LINE_ENDING)
        );
    }
}
