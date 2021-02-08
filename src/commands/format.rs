#![allow(dead_code)]

use std::io::Write;

use clap::{App, Arg, ArgMatches};
use fs_err::{read_to_string, OpenOptions};
use itertools::Itertools;

use crate::core::codegen::{codegen, CodegenOptions};
use crate::core::parser::*;
use crate::errors::MosResult;
use crate::LINE_ENDING;

enum Casing {
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

struct MnemonicOptions {
    casing: Casing,
}

struct Options {
    mnemonics: MnemonicOptions,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mnemonics: MnemonicOptions {
                casing: Casing::Lowercase,
            },
        }
    }
}

fn format_expression_factor(token: &ExpressionFactor, opts: &Options) -> String {
    match token {
        ExpressionFactor::Number(val, ty) => match ty {
            NumberType::Hex => {
                if *val < 256 {
                    format!("${:02x}", val)
                } else {
                    format!("${:04x}", val)
                }
            }
            NumberType::Bin => format!("%{:b}", val),
            NumberType::Dec => format!("{}", val),
        },
        ExpressionFactor::IdentifierValue(path, modifier) => {
            let modifier = match modifier {
                Some(m) => m.to_string(),
                None => "".to_string(),
            };
            format!("{}{}", modifier, path)
        }
        ExpressionFactor::ExprParens(inner) => {
            format!("[{}]", format_expression(&inner.data, opts))
        }
        ExpressionFactor::CurrentProgramCounter => "*".to_string(),
        ExpressionFactor::Ws(lhs, inner, rhs) => {
            format_ws(lhs, format_expression_factor(&inner.data, opts), rhs, opts)
        }
    }
}

fn format_expression(token: &Expression, opts: &Options) -> String {
    match token {
        Expression::Factor(factor, flags) => {
            format!("{}{}", flags, format_expression_factor(&factor.data, opts))
        }
        Expression::BinaryExpression(expr) => {
            format!("{} {} {}", expr.lhs.data, expr.op, expr.rhs.data)
        }
        Expression::Ws(lhs, inner, rhs) => {
            format_ws(lhs, format_expression(&inner.data, opts), rhs, opts)
        }
    }
}

fn indent_str(indent: usize) -> String {
    let mut str = "".to_string();
    for _ in 0..indent * 4 {
        str += " "
    }
    str
}

fn format_token(token: &Token, opts: &Options, indent: usize) -> String {
    match token {
        Token::Braces(tokens) => {
            let mut tokens = tokens
                .iter()
                .map(|t| format_token(&t.data, opts, indent + 1))
                .collect_vec()
                .join(LINE_ENDING)
                .trim_end() // if the scope ends with a newline due to for instance a 'rts', we trim it off here
                .to_string();
            if !tokens.is_empty() {
                tokens = format!("{le}{t}{le}", le = LINE_ENDING, t = tokens);
            }

            // Scopes always end with an extra newline
            format!(
                "{ind}{{{t}{ind}}}{le}",
                ind = indent_str(indent + 1),
                t = tokens,
                le = LINE_ENDING
            )
        }
        Token::Instruction(i) => {
            let mnem = opts.mnemonics.casing.format(&i.mnemonic.to_string());
            let operand = i
                .operand
                .as_ref()
                .map(|o| format_token(&o.data, opts, indent));

            let operand = operand.unwrap_or_else(|| "".to_string());
            let extra_newline = match &i.mnemonic {
                Mnemonic::Rts => LINE_ENDING,
                _ => "",
            };
            let ind = indent_str(indent + 1);
            format!("{}{} {}{}", ind, mnem, operand, extra_newline)
        }
        Token::Expression(e) => format_expression(e, opts),
        Token::Operand(o) => {
            let expr = format_expression(&o.expr.data, opts);
            let suffix = o
                .suffix
                .as_ref()
                .map(|o| format_token(&o.data, opts, indent))
                .unwrap_or_else(|| "".to_string());

            match &o.addressing_mode {
                AddressingMode::AbsoluteOrZP => format!("{}{}", expr, suffix),
                AddressingMode::Immediate => expr,
                AddressingMode::Implied => expr,
                AddressingMode::Indirect => format!("({}{})", expr, suffix),
                AddressingMode::OuterIndirect => format!("({}){}", expr, suffix),
            }
        }
        Token::IdentifierName(id) => {
            format!("{}", id)
        }
        Token::IdentifierPath(path) => path.to_str_vec().into_iter().join("."),
        Token::VariableDefinition(id, val, ty) => {
            let ty = match ty {
                VariableType::Variable => ".var",
                VariableType::Constant => ".const",
            };
            format!("{} {} = {}", ty, id, val.data)
        }
        Token::ProgramCounterDefinition(val) => {
            let ind = indent_str(indent + 1);
            format!("{}* = {}", ind, val.data)
        }
        Token::Data(expr, size) => {
            let expr = expr
                .iter()
                .map(|t| format_expression(&t.data, opts))
                .collect_vec()
                .join(", ");
            match size {
                1 => format!("{}.byte {}", indent_str(indent + 1), expr),
                2 => format!("{}.word {}", indent_str(indent + 1), expr),
                4 => format!("{}.dword {}", indent_str(indent + 1), expr),
                _ => panic!(),
            }
        }
        Token::Ws(lhs, inner, rhs) => {
            format_ws(lhs, format_token(&inner.data, opts, indent), rhs, opts)
        }
        Token::Label(id) => format!("{}:", id.0),
        Token::RegisterSuffix(reg) => match reg {
            Register::X => ", x".to_string(),
            Register::Y => ", y".to_string(),
        },
        Token::Definition(id, cfg) => {
            let cfg = cfg
                .as_ref()
                .map(|c| format_token(&c.data, opts, 0))
                .unwrap_or_else(|| "".to_string());
            format!(".define {} {}", format_token(&id.data, opts, 0), cfg)
        }
        Token::Segment(id, inner) => {
            let inner = match inner {
                Some(i) => format!(" {}", i.data),
                None => "".to_string(),
            };
            format!(".segment {}{}", id.data, inner)
        }
        Token::Config(cfg) => {
            let mut items = cfg
                .keys()
                .iter()
                .sorted()
                .map(|key| {
                    format!(
                        "{}{} = {}",
                        indent_str(indent + 1),
                        key,
                        format_token(&cfg.value(key).data, opts, 0)
                    )
                })
                .collect_vec()
                .join(LINE_ENDING)
                .trim_end()
                .to_string();
            if !items.is_empty() {
                items = format!("{le}{i}{le}", le = LINE_ENDING, i = items);
            }

            // Scopes always end with an extra newline
            format!(
                "{ind}{{{i}{ind}}}{le}",
                ind = indent_str(indent),
                i = items,
                le = LINE_ENDING
            )
        }
        Token::ConfigPair(_k, _v) => unimplemented!(),
        Token::If(ty, expr, if_, else_) => {
            let ty = match ty {
                IfType::IfExpr => ".if",
                IfType::IfDef(true) => ".ifdef",
                IfType::IfDef(false) => ".ifndef",
            };
            let expr = format_expression(&expr.data, opts);
            let if_ = format_token(&if_.data, opts, indent).trim().to_string();
            let else_ = match &else_ {
                Some(e) => {
                    let e = format_token(&e.data, opts, indent).trim().to_string();
                    format!(" else {}", e)
                }
                None => "".to_string(),
            };
            format!(
                "{ind}{ty} {expr} {if_}{else_}",
                ind = indent_str(indent + 1),
                ty = ty,
                expr = expr,
                if_ = if_,
                else_ = else_
            )
        }
        Token::Error => panic!("Formatting should not happen on ASTs containing errors"),
    }
}

fn format_ws(lhs: &[Comment], inner: String, rhs: &[Comment], _opts: &Options) -> String {
    let lhs = lhs.iter().map(|l| format!("{}", l)).collect_vec().join(" ");
    let rhs = rhs.iter().map(|l| format!("{}", l)).collect_vec().join(" ");
    let lhs_spacing = if lhs.is_empty() {
        "".to_string()
    } else {
        " ".to_string()
    };
    let rhs_spacing = if rhs.is_empty() {
        "".to_string()
    } else {
        " ".to_string()
    };
    format!("{}{}{}{}{}", lhs, lhs_spacing, inner, rhs_spacing, rhs)
}

fn format<'a>(ast: &[Located<'a, Token<'a>>], opts: &Options) -> String {
    ast.iter()
        .map(|lt| {
            let token = &lt.data;
            format_token(token, opts, 0)
        })
        .collect_vec()
        .join(LINE_ENDING)
        .trim_end()
        .to_string()
}

pub fn format_app() -> App<'static> {
    App::new("format").about("Formats input file(s)").arg(
        Arg::new("input")
            .about("Sets the input file(s) to use")
            .required(true)
            .multiple(true),
    )
}

pub fn format_command(args: &ArgMatches) -> MosResult<()> {
    let input_names = args.values_of("input").unwrap().collect_vec();

    for input_name in input_names {
        let source = read_to_string(input_name)?;
        let ast = parse(input_name, &source)?;
        let _code = codegen(ast.clone(), CodegenOptions::default())?;
        let formatted = format(&ast, &Options::default());
        let mut output_file = OpenOptions::new()
            .truncate(true)
            .write(true)
            .open(input_name)?;
        output_file.write_all(formatted.as_bytes())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use crate::commands::format::{format, Options};
    use crate::commands::{format_app, format_command};
    use crate::core::parser::parse;

    #[test]
    fn format_valid_code() -> Result<()> {
        let source = include_str!("../../test/cli/format/valid-unformatted.asm");
        let expected = include_str!("../../test/cli/format/valid-formatted.asm");
        let ast = parse("test.asm", source)?;
        assert_eq!(format(&ast, &Options::default()), expected);
        Ok(())
    }

    #[test]
    fn can_invoke_format_on_valid_file() -> Result<()> {
        let root = env!("CARGO_MANIFEST_DIR");
        let unformatted = &format!("{}/test/cli/format/valid-unformatted.asm", root);
        let formatted = &format!("{}/test/cli/format/valid-formatted.asm", root);
        let copied = &format!("{}/target/can_invoke_format.asm", root);
        std::fs::copy(unformatted, copied)?;

        let args = format_app().get_matches_from(vec!["format", copied]);
        format_command(&args)?;

        assert_eq!(
            std::fs::read_to_string(formatted)?,
            std::fs::read_to_string(copied)?
        );
        Ok(())
    }
}
