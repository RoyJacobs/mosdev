use crate::core::parser::{
    AddressingMode, ArgItem, Block, Expression, ExpressionFactor, Identifier, Located, Operand,
    ParseTree, Token, Trivia,
};
use itertools::Itertools;
use serde::Deserialize;
use std::fmt::{Debug, Display};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
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
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct MnemonicOptions {
    pub casing: Casing,
    pub register_casing: Casing,
}

impl Default for MnemonicOptions {
    fn default() -> Self {
        Self {
            casing: Casing::Lowercase,
            register_casing: Casing::Lowercase,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BracePosition {
    SameLine,
    NewLine,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct BraceOptions {
    pub position: BracePosition,
}

impl Default for BraceOptions {
    fn default() -> Self {
        Self {
            position: BracePosition::SameLine,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct WhitespaceOptions {
    pub indent: usize,
}

impl Default for WhitespaceOptions {
    fn default() -> Self {
        Self { indent: 4 }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "snake_case")]
pub struct FormattingOptions {
    pub mnemonics: MnemonicOptions,
    pub braces: BraceOptions,
    pub whitespace: WhitespaceOptions,
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

struct CodeFormatter {
    tree: Arc<ParseTree>,
    options: FormattingOptions,
    indent: usize,
}

impl CodeFormatter {
    fn new(tree: Arc<ParseTree>, options: FormattingOptions) -> Self {
        Self {
            tree,
            options,
            indent: 0,
        }
    }

    fn format(&mut self) -> String {
        let tree = self.tree.clone();
        self.format_tokens(tree.tokens()).trim_end().to_string()
    }

    fn format_tokens(&mut self, tokens: &[Token]) -> String {
        let leading_trivia = self.format_line(tokens, None).nl_if_not_empty();

        let formatted = tokens
            .iter()
            .enumerate()
            .map(|(token_idx, _)| {
                let fmt = self.format_line(tokens, Some(token_idx));

                // Do we need an additional newline?
                let extra_newline = match tokens.get(token_idx + 1) {
                    Some(next_token) => {
                        let token = &tokens[token_idx];

                        let (newline_if_same, newline_if_diff) = match token {
                            Token::If { .. } => (true, true),
                            Token::ProgramCounterDefinition { .. } => (false, false),
                            _ => (false, true),
                        };

                        let token_type = std::mem::discriminant(token);
                        let next_token_type = std::mem::discriminant(next_token);

                        (token_type == next_token_type && newline_if_same)
                            || (token_type != next_token_type && newline_if_diff)
                    }
                    None => false,
                };

                // Was the previous line a standalone comment? Then we'll skip the extra newline since we'll consider the previous
                // comment part of this token.
                //
                // If there wasn't even a previous token, then we also definitely don't want to skip extra lines.
                let allowed_to_have_extra_newlines = match fmt.last() {
                    Some(t) => !t.starts_with("/*") && !t.starts_with("//"),
                    None => false,
                };

                if extra_newline && allowed_to_have_extra_newlines {
                    fmt.nl().nl()
                } else {
                    fmt.nl()
                }
            })
            .collect_vec();
        let formatted = Fmt::flatten(formatted);
        let result = Fmt::new().push(leading_trivia).push(formatted);

        // Convert the Fmt into a string. Trim the start to prevent any unwanted newlines originating from the leading trivia.
        let result = result.join("").trim_start().to_string();

        // If we want to indent, split all the lines, indent them and join them up again.
        if self.indent > 0 {
            result
                .lines()
                .map(|line| {
                    if line.trim().is_empty() {
                        "".to_string()
                    } else {
                        format!("{}{}", indent_prefix(self.options.whitespace.indent), line)
                    }
                })
                .join("\n")
        } else {
            result
        }
    }

    fn format_line(&mut self, tokens: &[Token], token_idx: Option<usize>) -> Fmt {
        let token = match token_idx {
            Some(idx) => {
                let token = &tokens[idx];
                self.format_token(token)
            }
            None => Fmt::new(),
        };

        let trivia = {
            let trivia_idx = match token_idx {
                Some(idx) => idx + 1,
                None => 0,
            };
            match &tokens.get(trivia_idx) {
                Some(next_token) => next_token
                    .trivia()
                    .map(|t| self.format_trivia(t))
                    .unwrap_or_else(Fmt::new),
                None => Fmt::new(),
            }
        };

        Fmt::new()
            .push(token)
            .spc_if_next_if_not_empty()
            .push(trivia)
    }

    fn format_trivia(&mut self, trivia: &[Trivia]) -> Fmt {
        let mut fmt = Fmt::new();
        let mut prev_newlines = 0;
        for triv in trivia {
            fmt = match triv {
                Trivia::CStyle(comment) | Trivia::CppStyle(comment) => {
                    let fmt = match prev_newlines {
                        0 => fmt,
                        1 => fmt.nl(),
                        _ => {
                            // Limit the amount of newlines to 2
                            fmt.nl().nl()
                        }
                    };
                    prev_newlines = 0;
                    fmt.push(comment)
                }
                Trivia::Whitespace(_) => fmt,
                Trivia::NewLine => {
                    prev_newlines += 1;
                    fmt
                }
            };
        }
        fmt
    }

    fn format_optional_token<T: AsRef<Token>>(&mut self, token: &Option<T>) -> Fmt {
        token
            .as_ref()
            .map(|t| self.format_token(t.as_ref()))
            .unwrap_or_default()
    }

    fn format_token(&mut self, token: &Token) -> Fmt {
        match token {
            Token::Align { tag, value } => Fmt::new().push(&tag.data).spc().fmt(self, value),
            Token::Braces { block, .. } => Fmt::new().fmt(self, block),
            Token::Config(block) => Fmt::new().fmt(self, block),
            Token::ConfigPair { key, eq, value } => Fmt::new()
                .push(&key.data)
                .spc()
                .fmt(self, eq)
                .spc()
                .fmt(self, value),
            Token::Data { values, size } => Fmt::new()
                .push(size.data.to_string())
                .spc()
                .fmt(self, values),
            Token::Definition { tag, id, value } => Fmt::new()
                .push(&tag.data)
                .spc()
                .fmt(self, id)
                .spc()
                .fmt(self, value),
            Token::Eof(_) => Fmt::new(),
            Token::Error(_) => Fmt::new(),
            Token::Expression(expr) => Fmt::new().fmt(self, expr),
            Token::If {
                tag_if,
                value,
                if_,
                tag_else,
                else_,
                if_scope: _,
                else_scope: _,
            } => Fmt::new()
                .push(&tag_if.data)
                .spc()
                .fmt(self, value)
                .spc()
                .fmt(self, if_)
                .spc_if_next()
                .fmt(self, tag_else)
                .spc_if_next()
                .fmt(self, else_),
            Token::Include {
                tag,
                lquote,
                filename,
            } => Fmt::new()
                .push(&tag.data)
                .spc()
                .fmt(self, lquote)
                .fmt(self, filename)
                .push("\""),
            Token::Instruction(i) => Fmt::new()
                .push(
                    &self
                        .options
                        .mnemonics
                        .casing
                        .format(&i.mnemonic.data.to_string()),
                )
                .spc_if_next()
                .fmt(self, &i.operand),
            Token::Label { id, colon, block } => Fmt::new()
                .push(&id.data.to_string())
                .fmt(self, colon)
                .spc()
                .fmt(self, block),
            Token::MacroDefinition {
                tag,
                id,
                lparen,
                rparen,
                args,
                block,
            } => Fmt::new()
                .push(&tag.data)
                .spc()
                .fmt(self, id)
                .spc()
                .fmt(self, lparen)
                .fmt(self, args)
                .fmt(self, rparen)
                .fmt(self, block),
            Token::MacroInvocation {
                name,
                lparen,
                args,
                rparen,
            } => Fmt::new()
                .fmt(self, name)
                .fmt(self, lparen)
                .fmt(self, args)
                .fmt(self, rparen),
            Token::ProgramCounterDefinition { star, eq, value } => Fmt::new()
                .push(&star.data.to_string())
                .spc()
                .fmt(self, eq)
                .spc()
                .fmt(self, value),
            Token::Segment { tag, id, block } => Fmt::new()
                .push(&tag.data)
                .spc()
                .fmt(self, id)
                .spc_if_next()
                .fmt(self, block),
            Token::VariableDefinition { ty, id, eq, value } => Fmt::new()
                .push(&ty.data.to_string())
                .spc()
                .fmt(self, id)
                .spc()
                .fmt(self, eq)
                .spc()
                .fmt(self, value),
        }
    }

    fn format_operand(&mut self, operand: &Operand) -> Fmt {
        let suffix = operand
            .suffix
            .as_ref()
            .map(|s| {
                Fmt::new().fmt(self, &s.comma).fmt(
                    self,
                    s.register.map(|r| {
                        self.options
                            .mnemonics
                            .register_casing
                            .format(&r.to_string())
                    }),
                )
            })
            .unwrap_or_default();

        let fmt = Fmt::new()
            .fmt(self, &operand.lchar)
            .fmt(self, &operand.expr);

        match operand.addressing_mode {
            AddressingMode::Indirect => fmt.push(suffix).fmt(self, &operand.rchar),
            AddressingMode::OuterIndirect => fmt.fmt(self, &operand.rchar).push(suffix),
            _ => fmt.push(suffix),
        }
    }

    fn format_block(&mut self, block: &Block) -> Fmt {
        self.indent += 4;
        let inner = self.format_tokens(&block.inner);
        self.indent -= 4;
        Fmt::new()
            .fmt(self, &block.lparen.map(|_| "{\n"))
            .push(inner)
            .spc_if_next()
            .fmt(self, &block.rparen.map(|_| "\n}"))
    }

    fn format_expression(&mut self, expr: &Expression) -> Fmt {
        match expr {
            Expression::Factor {
                factor,
                flags: _,
                tag_not,
                tag_neg,
            } => Fmt::new()
                .fmt(self, tag_not)
                .fmt(self, tag_neg)
                .fmt(self, factor),
            Expression::BinaryExpression(b) => Fmt::new()
                .fmt(self, &b.lhs)
                .spc()
                .fmt(self, b.op.map(|op| op.to_string()))
                .spc()
                .fmt(self, &b.rhs),
        }
    }
    fn format_expression_factor(&mut self, factor: &ExpressionFactor) -> Fmt {
        match factor {
            ExpressionFactor::CurrentProgramCounter(star) => Fmt::new().fmt(self, star),
            ExpressionFactor::ExprParens {
                lparen,
                inner,
                rparen,
            } => Fmt::new()
                .fmt(self, lparen)
                .fmt(self, inner)
                .fmt(self, rparen),
            ExpressionFactor::FunctionCall {
                name,
                lparen,
                args,
                rparen,
            } => Fmt::new()
                .fmt(self, name)
                .fmt(self, lparen)
                .fmt(self, args)
                .fmt(self, rparen),
            ExpressionFactor::IdentifierValue { path, modifier } => Fmt::new()
                .fmt_opt(self, modifier, |m| m.map(|m| m.to_string()))
                .fmt(self, path.map(|p| p.to_string())),
            ExpressionFactor::Number { ty, value } => Fmt::new()
                .fmt(self, ty.map(|t| t.to_string()))
                .fmt(self, value.map(|v| v.to_string())),
        }
    }

    fn format_args(&mut self, args: &[ArgItem]) -> Fmt {
        let mut fmt = Fmt::new();
        for (expr, comma) in args {
            fmt = fmt.fmt(self, expr).fmt(self, comma);
        }
        fmt
    }

    fn format_optional_located<T: Display, L: AsRef<Located<T>>>(&mut self, lt: &Option<L>) -> Fmt {
        lt.as_ref()
            .map(|lt| self.format_located(lt.as_ref()))
            .unwrap_or_default()
    }

    fn format_located<T: Display>(&mut self, lt: &Located<T>) -> Fmt {
        let trivia = self.format_located_trivia(lt);
        Fmt::new()
            .push(trivia)
            .spc_if_next_if_not_empty()
            .push(lt.data.to_string())
    }

    fn format_located_token(&mut self, lt: &Located<Token>) -> Fmt {
        let trivia = self.format_located_trivia(lt);
        let data = self.format_token(&lt.data);
        Fmt::new()
            .push(trivia)
            .spc_if_next_if_not_empty()
            .push(data)
    }

    fn format_located_expression(&mut self, lt: &Located<Expression>) -> Fmt {
        let trivia = self.format_located_trivia(lt);
        let data = self.format_expression(&lt.data);
        Fmt::new()
            .push(trivia)
            .spc_if_next_if_not_empty()
            .push(data)
    }

    fn format_located_expression_factor(&mut self, lt: &Located<ExpressionFactor>) -> Fmt {
        let trivia = self.format_located_trivia(lt);
        let data = self.format_expression_factor(&lt.data);
        Fmt::new()
            .push(trivia)
            .spc_if_next_if_not_empty()
            .push(data)
    }

    fn format_located_trivia<T>(&mut self, lt: &Located<T>) -> Fmt {
        lt.trivia
            .as_ref()
            .map(|t| self.format_trivia(&t.data))
            .unwrap_or_default()
    }
}

#[derive(Debug)]
struct Fmt {
    lines: Vec<String>,
    debug: bool,
    spc_if_next: bool,
}

#[derive(Debug)]
struct FmtLine(Vec<String>);

impl FmtLine {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn starts_with(&self, pat: &str) -> bool {
        self.0
            .first()
            .map(|s| s.starts_with(pat))
            .unwrap_or_default()
    }
}

impl Into<FmtLine> for Fmt {
    fn into(self) -> FmtLine {
        FmtLine(self.lines)
    }
}

impl Into<FmtLine> for String {
    fn into(self) -> FmtLine {
        FmtLine(vec![self])
    }
}

impl Into<FmtLine> for &String {
    fn into(self) -> FmtLine {
        FmtLine(vec![self.clone()])
    }
}

impl Into<FmtLine> for &str {
    fn into(self) -> FmtLine {
        FmtLine(vec![self.to_string()])
    }
}

impl Into<FmtLine> for Vec<String> {
    fn into(self) -> FmtLine {
        FmtLine(self)
    }
}

impl Default for Fmt {
    fn default() -> Self {
        Fmt::new()
    }
}

trait Formattable {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt;
}

impl Formattable for &Box<Located<Token>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_token(self)
    }
}

impl Formattable for &Located<Token> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_token(self)
    }
}

impl Formattable for &Option<Box<Token>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_optional_token(self)
    }
}

impl Formattable for &Located<Expression> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_expression(self)
    }
}

impl Formattable for &Located<ExpressionFactor> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_expression_factor(self)
    }
}

impl Formattable for &Located<char> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located(self)
    }
}

impl Formattable for &Option<Located<char>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_optional_located(self)
    }
}

impl Formattable for &Located<Identifier> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located(&self.map(|i| i.to_string()))
    }
}

impl Formattable for &Located<&str> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located(self)
    }
}

impl Formattable for &Located<String> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located(self)
    }
}

impl Formattable for Located<String> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located(self)
    }
}

impl Formattable for &Option<Located<String>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        self.as_ref()
            .map(|s| formatter.format_located(s))
            .unwrap_or_default()
    }
}

impl Formattable for &Block {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_block(self)
    }
}

impl Formattable for &Option<Block> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        self.as_ref()
            .map(|b| formatter.format_block(b))
            .unwrap_or_default()
    }
}

impl Formattable for &Option<Operand> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        self.as_ref()
            .map(|o| formatter.format_operand(o))
            .unwrap_or_default()
    }
}

impl Formattable for &Expression {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_expression(self)
    }
}

impl Formattable for &Box<Located<Expression>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_expression(self)
    }
}

impl Formattable for &Box<Located<ExpressionFactor>> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_located_expression_factor(self)
    }
}

impl Formattable for &Vec<ArgItem> {
    fn format(&self, formatter: &mut CodeFormatter) -> Fmt {
        formatter.format_args(self)
    }
}

impl Fmt {
    fn new() -> Self {
        Self {
            lines: vec![],
            debug: false,
            spc_if_next: false,
        }
    }

    #[allow(dead_code)]
    fn debug(mut self) -> Self {
        self.debug = true;
        log::trace!("== debug =============================");
        self
    }

    #[allow(dead_code)]
    fn debug_end(mut self) -> Self {
        log::trace!("== debug end =========================");
        self.debug = false;
        self
    }

    #[allow(dead_code)]
    fn trace(&self) {
        log::trace!("== trace =============================");
        for line in &self.lines {
            log::trace!("{}", line);
        }
        log::trace!("== trace end =========================");
    }

    fn last(&self) -> Option<&String> {
        self.lines.last()
    }

    fn push<D: Into<FmtLine>>(mut self, data: D) -> Self {
        let was_spc_if_next = self.spc_if_next;
        self.spc_if_next = false;

        let data = data.into();
        if data.is_empty() {
            return self;
        }

        let should_add_space = !data.starts_with("\n");
        let data = if was_spc_if_next && should_add_space {
            let mut r = vec![];
            r.push(" ".to_string());
            r.extend(data.0);
            FmtLine(r)
        } else {
            data
        };

        if self.debug {
            log::trace!("Inserting: {:?}", &data);
        }
        self.lines.extend(data.0);
        self
    }

    fn fmt<D: Formattable>(self, formatter: &mut CodeFormatter, data: D) -> Self {
        self.push(data.format(formatter))
    }

    fn fmt_opt<D, F: Fn(&D) -> T, T: Formattable>(
        self,
        formatter: &mut CodeFormatter,
        data: &Option<D>,
        map: F,
    ) -> Self {
        if let Some(d) = data {
            let d = map(d);
            self.push(d.format(formatter))
        } else {
            self
        }
    }

    fn spc(self) -> Self {
        self.push(" ")
    }

    fn spc_if_next(mut self) -> Self {
        self.spc_if_next = true;
        self
    }

    fn spc_if_next_if_not_empty(mut self) -> Self {
        if !self.is_empty() {
            self.spc_if_next = true;
        }
        self
    }

    fn nl(self) -> Self {
        self.push("\n")
    }

    fn nl_if_not_empty(self) -> Self {
        if !self.is_empty() {
            self.nl()
        } else {
            self
        }
    }

    fn is_empty(&self) -> bool {
        self.lines.iter().all(|line| line.is_empty())
    }

    fn join(self, join: &str) -> String {
        self.lines.into_iter().join(join)
    }

    fn flatten(fmts: Vec<Fmt>) -> Fmt {
        let mut result = Fmt::new();
        for fmt in fmts {
            result = result.push(fmt.lines);
        }
        result
    }
}

pub fn format(ast: Arc<ParseTree>, options: FormattingOptions) -> String {
    let mut fmt = CodeFormatter::new(ast, options);
    fmt.format()
}

fn indent_prefix(indent: usize) -> String {
    let mut indent_prefix = "".to_string();
    for _ in 0..indent {
        indent_prefix += " "
    }
    indent_prefix
}

#[cfg(test)]
mod tests {
    use super::{format, FormattingOptions};
    use crate::core::parser::parse_or_err;
    use crate::errors::{MosError, MosResult};
    use crate::formatting::BracePosition;
    use itertools::Itertools;

    #[test]
    fn newline_braces() -> MosResult<()> {
        let source = "{nop}";
        let expected = "{\n    nop\n}";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let mut opts = FormattingOptions::default();
        opts.braces.position = BracePosition::NewLine;
        let actual = format(ast, opts);
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn no_newline_if_same() -> MosResult<()> {
        let source = "nop\nnop\n// hello\nnop\n\n// foo\n.align 8\n.align 16";
        let expected = "nop\nnop\n// hello\nnop\n\n// foo\n.align 8\n.align 16";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn keep_standalone_comments() -> MosResult<()> {
        let source = "nop\n\n// standalone\nnop";
        let expected = "nop\n\n// standalone\nnop";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn keep_grouped_comments() -> MosResult<()> {
        let source = "// a\n// b";
        let expected = "// a\n// b";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn block_indentation() -> MosResult<()> {
        let source = "{stx data\n           .if test { nop  }      }";
        let expected = "{\n    stx data\n\n    .if test {\n        nop\n    }\n}";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn simple_leading_trivia_block_comments() -> MosResult<()> {
        let source = "{\n/* nice*/\nnop}";
        let expected = "{\n    /* nice*/\n    nop\n}";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn simple_block_comments() -> MosResult<()> {
        let source = "{nop/* nice*/}";
        let expected = "{\n    nop /* nice*/\n}";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn block_comments() -> MosResult<()> {
        let source = r"data: {          /* here it is */
 .byte          1// hello
 .word  4
 nop}";
        let expected = r"data: {
    /* here it is */
    .byte 1 // hello
    .word 4

    nop
}";
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn default_formatting_options() -> MosResult<()> {
        let source = include_str!("../../test/cli/format/valid-unformatted.asm");
        let expected = include_str!("../../test/cli/format/valid-formatted.asm");
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
        eq(actual, expected);
        Ok(())
    }

    #[test]
    fn roundtrip_formatting() -> MosResult<()> {
        let source = include_str!("../../test/cli/format/valid-formatted.asm");
        let expected = include_str!("../../test/cli/format/valid-formatted.asm");
        let ast = parse_or_err("test.asm".as_ref(), source)?;
        let actual = format(ast, FormattingOptions::default());
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
