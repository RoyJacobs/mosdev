use crate::core::parser::config_map::ConfigMap;
use crate::core::parser::mnemonic::Mnemonic;
use crate::core::parser::{Identifier, IdentifierPath, ParseError};
use codemap::{CodeMap, File, Span};
use itertools::Itertools;
use std::cell::RefCell;
use std::fmt::{Binary, Debug, Display, Formatter, LowerHex};
use std::rc::Rc;
use std::sync::Arc;

/// A span containing a fragment of the source text and the location of this fragment within the source
pub type LocatedSpan<'a> = nom_locate::LocatedSpan<&'a str, State>;

/// A convenience wrapper around nom's own [nom::IResult] to make use of our own [LocatedSpan] type
pub type IResult<'a, T> = nom::IResult<LocatedSpan<'a>, T>;

/// An item in a comma-separated list
pub type ArgItem = (Located<Expression>, Option<Located<char>>);

/// The result of parsing
pub struct ParseTree {
    code_map: CodeMap,
    files: Vec<Arc<File>>,
    tokens: Vec<Token>,
}

impl Debug for ParseTree {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "[ParseTree]")
    }
}

impl ParseTree {
    pub fn new(code_map: CodeMap, files: Vec<Arc<File>>, tokens: Vec<Token>) -> Self {
        Self {
            code_map,
            files,
            tokens,
        }
    }

    pub fn code_map(&self) -> &CodeMap {
        &self.code_map
    }

    pub fn files(&self) -> &Vec<Arc<File>> {
        &self.files
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
}

/// The state of the parser
#[derive(Clone, Debug)]
pub struct State {
    /// Codemap
    pub code_map: Rc<RefCell<CodeMap>>,

    /// All files that were parsed
    pub files: Rc<RefCell<Vec<Arc<codemap::File>>>>,

    /// Which file are we parsing?
    pub file: Arc<codemap::File>,

    /// Which errors did we encounter?
    pub errors: Rc<RefCell<Vec<ParseError>>>,

    /// Should the next error be ignored?
    ignore_next_error: Rc<RefCell<bool>>,

    /// Current anonymous scope index
    anonymous_scope_index: Rc<RefCell<usize>>,
}

impl State {
    pub fn new<P: Into<String>, S: Into<String>>(path: P, source: S) -> Self {
        let mut code_map = CodeMap::new();
        let file = code_map.add_file(path.into(), source.into());

        Self {
            code_map: Rc::new(RefCell::new(code_map)),
            files: Rc::new(RefCell::new(vec![file.clone()])),
            file,
            errors: Rc::new(RefCell::new(Vec::new())),
            ignore_next_error: Rc::new(RefCell::new(false)),
            anonymous_scope_index: Rc::new(RefCell::new(0)),
        }
    }

    /// Mark the next reported error to be ignored (since it may be redundant or something)
    pub fn ignore_next_error(&self) {
        log::trace!("Ignoring next error");
        *self.ignore_next_error.borrow_mut() = true;
    }

    /// When there is an error during parsing we don't want to fail. Instead, we continue but log the error via this method
    pub fn report_error(&self, error: ParseError) {
        if *self.ignore_next_error.borrow() {
            log::trace!("Ignoring error: {:?}", error);
            *self.ignore_next_error.borrow_mut() = false;
        } else {
            log::trace!("Pushing error: {:?}", error);
            self.errors.borrow_mut().push(error);
        }
    }

    pub fn new_anonymous_scope(&self) -> Identifier {
        let mut index = self.anonymous_scope_index.borrow_mut();
        *index += 1;
        Identifier::anonymous(*index)
    }
}

/// Any trivia we may encounter during parsing
#[derive(Clone, Debug, PartialEq)]
pub enum Trivia {
    /// One or more spaces or tabs
    Whitespace(String),
    /// A newline
    NewLine,
    /// A C-style comment: `/* foo */`
    CStyle(String),
    /// A C++-style comment: `// foo`
    CppStyle(String),
}

/// Registers used for indexing
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum IndexRegister {
    X,
    Y,
}

impl Display for IndexRegister {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::X => write!(f, "X"),
            Self::Y => write!(f, "Y"),
        }
    }
}

/// A 6502 instruction
#[derive(Debug, PartialEq)]
pub struct Instruction {
    pub mnemonic: Located<Mnemonic>,
    /// The operand is optional because some instructions (e.g. `NOP`) don't have an operand.
    pub operand: Option<Operand>,
}

/// The addressing mode for the instruction
#[derive(Debug, Clone, PartialEq)]
pub enum AddressingMode {
    /// Absolute or Zero-Page addressing (e.g. `LDA $34`)
    AbsoluteOrZP,
    /// Immediate addressing (e.g. `LDA #123`)
    Immediate,
    /// Implied (e.g. `NOP`)
    Implied,
    /// Indirect (e.g. `LDA ($12,X)`
    Indirect,
    /// Outer indirection (e.g. `LDA ($12),Y`)
    OuterIndirect,
}

/// The operand of an instruction
#[derive(Debug, PartialEq)]
pub struct Operand {
    pub expr: Located<Expression>,
    pub lchar: Option<Located<char>>,
    pub rchar: Option<Located<char>>,
    pub addressing_mode: AddressingMode,
    pub suffix: Option<RegisterSuffix>,
}

/// The optional register suffix of an operand
#[derive(Debug, PartialEq)]
pub struct RegisterSuffix {
    pub comma: Located<char>,
    pub register: Located<IndexRegister>,
}

/// A number, which can be hexadecimal (`$23AB`), decimal (`123`) or binary (`%11011`)
#[derive(Debug, Clone, PartialEq)]
pub enum NumberType {
    Hex,
    Dec,
    Bin,
}

impl Display for NumberType {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            NumberType::Hex => write!(f, "$"),
            NumberType::Dec => Ok(()),
            NumberType::Bin => write!(f, "%"),
        }
    }
}

/// An address modifier which can be used to get the low (`<`) or high (`>`) byte of an address
#[derive(Debug, Clone, PartialEq)]
pub enum AddressModifier {
    HighByte,
    LowByte,
}

impl Display for AddressModifier {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::LowByte => write!(f, "<"),
            Self::HighByte => write!(f, ">"),
        }
    }
}

/// Any kind of binary operation that can be found in an expression
#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    /// Addition
    Add,
    /// Subtraction
    Sub,
    /// Multiplication
    Mul,
    /// Division
    Div,
    /// Shift left
    Shl,
    /// Shift right
    Shr,
    /// Exclusive or
    Xor,
    /// Equals
    Eq,
    /// Not equals
    Ne,
    /// Greater than
    Gt,
    /// Greater than or equal
    GtEq,
    /// Less than
    Lt,
    /// Less than or equal
    LtEq,
    /// And
    And,
    /// Or
    Or,
}

impl Display for BinaryOp {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match &self {
            BinaryOp::Add => write!(f, "+"),
            BinaryOp::Sub => write!(f, "-"),
            BinaryOp::Mul => write!(f, "*"),
            BinaryOp::Div => write!(f, "/"),
            BinaryOp::Shl => write!(f, "<<"),
            BinaryOp::Shr => write!(f, ">>"),
            BinaryOp::Xor => write!(f, "^"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::Ne => write!(f, "!="),
            BinaryOp::Gt => write!(f, ">"),
            BinaryOp::GtEq => write!(f, ">="),
            BinaryOp::Lt => write!(f, "<"),
            BinaryOp::LtEq => write!(f, "<="),
            BinaryOp::And => write!(f, "&&"),
            BinaryOp::Or => write!(f, "||"),
        }
    }
}

/// A binary expression of the form `lhs op rhs`
#[derive(Debug, PartialEq)]
pub struct BinaryExpression {
    pub op: Located<BinaryOp>,
    pub lhs: Box<Located<Expression>>,
    pub rhs: Box<Located<Expression>>,
}

/// A factor that can be used on either side of an expression operation
#[derive(Debug, PartialEq)]
pub enum ExpressionFactor {
    CurrentProgramCounter(Located<char>),
    ExprParens {
        lparen: Located<char>,
        inner: Box<Located<Expression>>,
        rparen: Located<char>,
    },
    FunctionCall {
        name: Located<Identifier>,
        lparen: Located<char>,
        args: Vec<ArgItem>,
        rparen: Located<char>,
    },
    IdentifierValue {
        path: Located<IdentifierPath>,
        modifier: Option<Located<AddressModifier>>,
    },
    Number {
        ty: Located<NumberType>,
        value: Located<Number>,
    },
}

/// A wrapper that stores the original number string and its radix, so that any zero-prefixes are kept
#[derive(Debug, PartialEq)]
pub struct Number {
    radix: u32,
    data: String,
}

impl Display for Number {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}", self.data)
    }
}

impl Number {
    pub fn value(&self) -> i64 {
        i64::from_str_radix(&self.data, self.radix).ok().unwrap()
    }

    pub fn from_type<S: AsRef<str>>(ty: NumberType, data: S) -> Self {
        let radix = match ty {
            NumberType::Hex => 16,
            NumberType::Dec => 10,
            NumberType::Bin => 2,
        };
        Self {
            radix,
            data: data.as_ref().into(),
        }
    }
}

bitflags::bitflags! {
    /// Flags that can be used to modify the [ExpressionFactor] the flags belong to
    pub struct ExpressionFactorFlags: u8 {
        const NOT = 0b00000001;
        const NEG = 0b00000010;
    }
}

impl Display for ExpressionFactorFlags {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let not = if self.contains(ExpressionFactorFlags::NOT) {
            "!"
        } else {
            ""
        };
        let neg = if self.contains(ExpressionFactorFlags::NEG) {
            "-"
        } else {
            ""
        };
        write!(f, "{}{}", not, neg)
    }
}

/// An expression consists of one or more factors, possibly contained within a binary (sub)expression
#[derive(Debug, PartialEq)]
pub enum Expression {
    BinaryExpression(BinaryExpression),
    Factor {
        factor: Box<Located<ExpressionFactor>>,
        flags: ExpressionFactorFlags,
        tag_not: Option<Located<char>>,
        tag_neg: Option<Located<char>>,
    },
}

impl Expression {
    /// Grab the trivia that comes before the first item in the token (e.g. stuff that could be to the right of the previous token)
    pub fn trivia(&self) -> Option<&Vec<Trivia>> {
        let t = match self {
            Expression::BinaryExpression(expr) => &expr.lhs.trivia,
            Expression::Factor { factor, .. } => &factor.trivia,
        };

        t.as_ref().map(|t| &t.data)
    }
}

/// User-defined variables
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum VariableType {
    Constant,
    Variable,
}

impl Display for VariableType {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            VariableType::Constant => write!(f, ".const"),
            VariableType::Variable => write!(f, ".var"),
        }
    }
}

/// The size of a data directive (e.g. `.byte 1, 2, 3`)
#[derive(Debug, Clone, PartialEq)]
pub enum DataSize {
    Byte,
    Word,
    Dword,
}

impl DataSize {
    pub fn byte_len(&self) -> usize {
        match self {
            DataSize::Byte => 1,
            DataSize::Word => 2,
            DataSize::Dword => 4,
        }
    }
}

impl Display for DataSize {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::Byte => write!(f, ".byte"),
            Self::Word => write!(f, ".word"),
            Self::Dword => write!(f, ".dword"),
        }
    }
}

/// A block of tokens
#[derive(Debug, PartialEq)]
pub struct Block {
    pub lparen: Located<char>,
    pub inner: Vec<Token>,
    pub rparen: Located<char>,
}

/// Tokens that, together, make up all possible source text
#[derive(Debug, PartialEq)]
pub enum Token {
    Align {
        tag: Located<String>,
        value: Located<Expression>,
    },
    Braces {
        block: Block,
        scope: Identifier,
    },
    Config(Block),
    ConfigPair {
        key: Located<String>,
        eq: Located<char>,
        value: Box<Located<Token>>,
    },
    Data {
        values: Vec<ArgItem>,
        size: Located<DataSize>,
    },
    Definition {
        tag: Located<String>,
        id: Located<Identifier>,
        value: Option<Box<Token>>,
    },
    Eof(Located<()>),
    Error(Located<()>),
    Expression(Expression),
    If {
        tag_if: Located<String>,
        value: Located<Expression>,
        if_: Block,
        if_scope: Box<Identifier>,
        tag_else: Option<Located<String>>,
        else_: Option<Block>,
        else_scope: Box<Identifier>,
    },
    Include {
        tag: Located<String>,
        lquote: Located<char>,
        filename: Located<String>,
    },
    Instruction(Instruction),
    Label {
        id: Located<Identifier>,
        colon: Located<char>,
        block: Option<Block>,
    },
    ProgramCounterDefinition {
        star: Located<char>,
        eq: Located<char>,
        value: Located<Expression>,
    },
    Segment {
        tag: Located<String>,
        id: Located<Identifier>,
        block: Option<Block>,
    },
    VariableDefinition {
        ty: Located<VariableType>,
        id: Located<Identifier>,
        eq: Located<char>,
        value: Located<Expression>,
    },
}

impl Token {
    /// Grab the trivia that comes before the first item in the token (e.g. stuff that could be to the right of the previous token)
    pub fn trivia(&self) -> Option<&Vec<Trivia>> {
        let t = match self {
            Token::Align { tag, .. } => &tag.trivia,
            Token::Braces { block, .. } => &block.lparen.trivia,
            Token::Config(block) => &block.lparen.trivia,
            Token::ConfigPair { key, .. } => &key.trivia,
            Token::Data { size, .. } => &size.trivia,
            Token::Definition { tag, .. } => &tag.trivia,
            Token::Eof(empty) => &empty.trivia,
            Token::Error(_) => panic!(),
            Token::Expression(expr) => {
                return expr.trivia();
            }
            Token::If { tag_if, .. } => &tag_if.trivia,
            Token::Instruction(i) => &i.mnemonic.trivia,
            Token::Include { tag, .. } => &tag.trivia,
            Token::Label { id, .. } => &id.trivia,
            Token::ProgramCounterDefinition { star, .. } => &star.trivia,
            Token::Segment { tag, .. } => &tag.trivia,
            Token::VariableDefinition { ty, .. } => &ty.trivia,
        };

        t.as_ref().map(|t| &t.data)
    }

    pub(crate) fn as_config_map(&self) -> ConfigMap {
        match self {
            Token::Config(block) => ConfigMap::new(block.lparen.span, &block.inner),
            _ => panic!(),
        }
    }

    pub(crate) fn as_expression(&self) -> &Expression {
        match self {
            Token::Expression(expr) => &expr,
            _ => panic!(),
        }
    }

    pub(crate) fn as_factor(&self) -> &ExpressionFactor {
        match self {
            Token::Expression(Expression::Factor { factor, .. }) => &factor.data,
            _ => panic!(),
        }
    }

    pub(crate) fn try_as_factor(&self) -> Option<&ExpressionFactor> {
        match self {
            Token::Expression(Expression::Factor { factor, .. }) => Some(&factor.data),
            _ => None,
        }
    }
}

/// A wrapper around any kind of data which adds span information and any trivia.
///
/// During parsing the trivia that is included with tokens is any trivia that is located to the left of the token.
#[derive(Debug, Clone, PartialEq)]
pub struct Located<T> {
    pub span: Span,
    pub data: T,
    pub trivia: Option<Box<Located<Vec<Trivia>>>>,
}

impl<T> Into<Span> for Located<T> {
    fn into(self) -> Span {
        self.span
    }
}

impl<T> Into<Span> for &Located<T> {
    fn into(self) -> Span {
        self.span
    }
}

impl<T> AsRef<Located<T>> for Located<T> {
    fn as_ref(&self) -> &Located<T> {
        &self
    }
}

impl<T> Located<Located<T>> {
    /// Remove one layer of nesting for nested located types
    pub fn flatten(self) -> Located<T> {
        Located {
            span: self.data.span,
            data: self.data.data,
            trivia: self.trivia,
        }
    }
}

impl<T> Located<T> {
    /// Borrow the [Located] and map the data to something else
    pub fn map<U, F: Fn(&T) -> U>(&self, map_fn: F) -> Located<U> {
        Located::new_with_trivia(self.span, map_fn(&self.data), self.trivia.clone())
    }

    /// Take ownership of the [Located] and map the data into something else
    pub fn map_into<U, F: FnOnce(T) -> U>(self, map_fn: F) -> Located<U> {
        Located::new_with_trivia(self.span, map_fn(self.data), self.trivia)
    }

    pub fn new(span: Span, data: T) -> Self {
        Self {
            span,
            data,
            trivia: None,
        }
    }

    pub fn new_with_trivia(span: Span, data: T, trivia: Option<Box<Located<Vec<Trivia>>>>) -> Self {
        Self { span, data, trivia }
    }
}

impl Display for Mnemonic {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let m = format!("{:?}", self).to_uppercase();
        write!(f, "{}", m)
    }
}

impl Display for Trivia {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Trivia::Whitespace(str) => write!(f, "{}", str),
            Trivia::NewLine => writeln!(f),
            Trivia::CStyle(str) => write!(f, "{}", str),
            Trivia::CppStyle(str) => write!(f, "{}", str),
        }
    }
}

impl Display for ExpressionFactor {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::CurrentProgramCounter(star) => write!(f, "{}", star),
            Self::ExprParens {
                lparen,
                inner,
                rparen,
            } => {
                write!(f, "{}{}{}", lparen, inner, rparen)
            }
            Self::FunctionCall {
                name,
                lparen,
                args,
                rparen,
            } => {
                write!(f, "{}{}{}{}", name, lparen, format_arglist(args), rparen)
            }
            Self::IdentifierValue { path, modifier } => {
                let modifier = match modifier {
                    Some(m) => m.to_string(),
                    None => "".to_string(),
                };
                write!(f, "{}{}", modifier, path)
            }
            Self::Number { ty, value } => write!(f, "{}{}", ty, value),
        }
    }
}

/// Formats a list of [ArgItem], using its contained trivia to add the separating commas, etc
fn format_arglist(args: &[ArgItem]) -> String {
    args.iter()
        .map(|(arg, comma)| {
            let comma = match comma {
                Some(c) => format!("{}", c),
                None => "".to_string(),
            };
            format!("{}{}", arg, comma)
        })
        .join("")
}

/// Formats all the [Trivia] enclosed in a [Located]
fn format_trivia(trivia: &Option<Box<Located<Vec<Trivia>>>>) -> String {
    trivia
        .as_ref()
        .map(|t| t.data.iter().map(|c| format!("{}", c)).join(""))
        .unwrap_or_else(|| "".to_string())
}

impl<'a, T: Display> Display for Located<T> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}{}", format_trivia(&self.trivia), &self.data)
    }
}

impl<'a, T: Display + LowerHex> LowerHex for Located<T> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}{:x}", format_trivia(&self.trivia), &self.data)
    }
}

impl<'a, T: Display + Binary> Binary for Located<T> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}{:b}", format_trivia(&self.trivia), &self.data)
    }
}

impl Display for Expression {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::BinaryExpression(expr) => {
                write!(f, "{}{}{}", expr.lhs, expr.op, expr.rhs)
            }
            Self::Factor {
                factor,
                tag_not,
                tag_neg,
                ..
            } => {
                let not = match tag_not {
                    Some(n) => format!("{}", n),
                    None => "".to_string(),
                };
                let neg = match tag_neg {
                    Some(n) => format!("{}", n),
                    None => "".to_string(),
                };
                write!(f, "{}{}{}", not, neg, factor)
            }
        }
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let inner = self
            .inner
            .iter()
            .map(|t| format!("{}", t))
            .collect_vec()
            .join("");
        write!(f, "{}{}{}", self.lparen, inner, self.rparen)
    }
}

impl Display for Token {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Token::Align { tag, value } => {
                write!(f, "{}{}", format!("{}", tag).to_uppercase(), value)
            }
            Token::Braces { block, .. } | Token::Config(block) => {
                write!(f, "{}", block)
            }
            Token::ConfigPair { key, eq, value } => {
                write!(f, "{}{}{}", key, eq, value)
            }
            Token::Data { values, size } => {
                write!(
                    f,
                    "{}{}",
                    format!("{}", size).to_uppercase(),
                    format_arglist(values)
                )
            }
            Token::Definition { tag, id, value } => {
                let value = value
                    .as_ref()
                    .map(|c| format!("{}", c))
                    .unwrap_or_else(|| "".to_string());
                write!(f, "{}{}{}", format!("{}", tag).to_uppercase(), id, value)
            }
            Token::Eof(triv) | Token::Error(triv) => {
                write!(f, "{}", format_trivia(&triv.trivia))
            }
            Token::Expression(e) => write!(f, "{}", e),
            Token::If {
                tag_if,
                value,
                if_,
                tag_else,
                else_,
                if_scope: _,
                else_scope: _,
            } => {
                let else_ = match (tag_else, else_) {
                    (Some(tag), Some(e)) => format!("{}{}", format!("{}", tag).to_uppercase(), e),
                    _ => "".to_string(),
                };
                write!(
                    f,
                    "{}{}{}{}",
                    format!("{}", tag_if).to_uppercase(),
                    value,
                    if_,
                    else_
                )
            }
            Token::Include {
                tag,
                lquote,
                filename,
            } => {
                write!(
                    f,
                    "{}{}{}\"",
                    tag.map(|t| t.to_uppercase()),
                    lquote,
                    filename,
                )
            }
            Token::Instruction(i) => match &i.operand {
                Some(o) => {
                    let suffix = match &o.suffix {
                        Some(s) => format!("{}{}", s.comma, s.register.to_string().to_uppercase()),
                        None => "".to_string(),
                    };

                    let operand = match &o.addressing_mode {
                        AddressingMode::AbsoluteOrZP => {
                            format!("{}{}", o.expr, suffix)
                        }
                        AddressingMode::Immediate => {
                            format!("{}{}", o.lchar.as_ref().unwrap(), o.expr)
                        }
                        AddressingMode::Implied => "".to_string(),
                        AddressingMode::OuterIndirect => {
                            format!(
                                "{}{}{}{}",
                                o.lchar.as_ref().unwrap(),
                                o.expr,
                                o.rchar.as_ref().unwrap(),
                                suffix
                            )
                        }
                        AddressingMode::Indirect => {
                            format!(
                                "{}{}{}{}",
                                o.lchar.as_ref().unwrap(),
                                o.expr,
                                suffix,
                                o.rchar.as_ref().unwrap()
                            )
                        }
                    };

                    write!(f, "{}{}", i.mnemonic, operand)
                }
                None => write!(f, "{}", i.mnemonic),
            },
            Token::Label { id, colon, block } => {
                let block = match block {
                    Some(s) => format!("{}", s),
                    None => "".to_string(),
                };
                write!(f, "{}{}{}", id, colon, block)
            }
            Token::ProgramCounterDefinition { star, eq, value } => {
                write!(f, "{}{}{}", star, eq, value)
            }
            Token::Segment { tag, id, block } => {
                let block = match block {
                    Some(i) => format!("{}", i),
                    None => "".to_string(),
                };
                write!(f, "{}{}{}", format!("{}", tag).to_uppercase(), id, block)
            }
            Token::VariableDefinition { ty, id, eq, value } => {
                write!(
                    f,
                    "{}{}{}{}",
                    format!("{}", ty).to_uppercase(),
                    id,
                    eq,
                    value
                )
            }
        }
    }
}
