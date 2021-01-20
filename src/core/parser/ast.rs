use crate::core::parser::mnemonic::Mnemonic;
use crate::errors::MosError;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::rc::Rc;

pub type LocatedSpan<'a> = nom_locate::LocatedSpan<&'a str, State<'a>>;
pub type IResult<'a, T> = nom::IResult<LocatedSpan<'a>, T>;

#[derive(Clone, Debug)]
pub struct State<'a> {
    pub filename: Rc<String>,
    pub errors: &'a RefCell<Vec<MosError>>,
}

impl<'a> State<'a> {
    pub fn report_error(&self, error: MosError) {
        self.errors.borrow_mut().push(error);
    }
}

#[derive(Debug)]
pub enum Comment {
    CStyle(String),
    CppStyle(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Identifier(pub String);

impl Display for Identifier {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub path: Rc<String>,
    pub line: u32,
    pub column: u32,
}

impl Location {
    pub fn unknown() -> Self {
        Self {
            path: Rc::new("unknown".to_string()),
            line: 0,
            column: 0,
        }
    }
}

impl<'a> From<&LocatedSpan<'a>> for Location {
    fn from(span: &LocatedSpan) -> Self {
        Self {
            path: span.extra.filename.clone(),
            line: span.location_line(),
            column: span.get_column() as u32,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum Register {
    X,
    Y,
}

#[derive(Debug)]
pub struct Instruction {
    pub mnemonic: Mnemonic,
    pub operand: Option<Box<Located<Token>>>,
}

#[derive(Debug)]
pub enum AddressingMode {
    AbsoluteOrZP,
    Immediate,
    Implied,
    Indirect,
    OuterIndirect,
}

#[derive(Debug)]
pub struct Operand {
    pub expr: Box<Located<Expression>>,
    pub addressing_mode: AddressingMode,
    pub suffix: Option<Box<Located<Token>>>,
}

#[derive(Debug)]
pub enum NumberType {
    Hex,
    Dec,
}

#[derive(Debug)]
pub enum Expression {
    Identifier(Identifier),
    Number(usize, NumberType),
    ExprParens(Box<Located<Expression>>),
    BinaryAdd(Box<Located<Expression>>, Box<Located<Expression>>),
    BinarySub(Box<Located<Expression>>, Box<Located<Expression>>),
    BinaryMul(Box<Located<Expression>>, Box<Located<Expression>>),
    BinaryDiv(Box<Located<Expression>>, Box<Located<Expression>>),
    Ws(Vec<Comment>, Box<Located<Expression>>, Vec<Comment>),
}

#[derive(Debug)]
pub enum Token {
    Label(Identifier),
    Instruction(Instruction),
    Operand(Operand),
    RegisterSuffix(Register),
    Ws(Vec<Comment>, Box<Located<Token>>, Vec<Comment>),
    Data(Option<Box<Located<Expression>>>, usize),
    Error,
}

#[derive(Debug)]
pub struct Located<T: CanWrapWhitespace> {
    pub location: Location,
    pub data: T,
}

pub trait CanWrapWhitespace {
    fn strip_whitespace(self) -> Self;
    fn wrap_inner(lhs: Vec<Comment>, inner: Box<Located<Self>>, rhs: Vec<Comment>) -> Self
    where
        Self: Sized;
}

impl<T: CanWrapWhitespace> Located<T> {
    pub fn from<L: Into<Location>>(location: L, data: T) -> Self {
        Self {
            location: location.into(),
            data,
        }
    }

    pub fn strip_whitespace(self) -> Self {
        Self {
            data: self.data.strip_whitespace(),
            ..self
        }
    }
}

fn sob<T: CanWrapWhitespace>(token: Option<Box<Located<T>>>) -> Option<Box<Located<T>>> {
    token.map(|t| Box::new(t.strip_whitespace()))
}

#[allow(clippy::boxed_local)]
fn sb<T: CanWrapWhitespace>(t: Box<Located<T>>) -> Box<Located<T>> {
    Box::new(t.strip_whitespace())
}

impl CanWrapWhitespace for Token {
    fn strip_whitespace(self) -> Self {
        match self {
            Token::Instruction(i) => Token::Instruction(Instruction {
                operand: sob(i.operand),
                ..i
            }),
            Token::Operand(o) => Token::Operand(Operand {
                expr: sb(o.expr),
                suffix: sob(o.suffix),
                ..o
            }),
            Token::Data(inner, size) => Token::Data(sob(inner), size),
            Token::Ws(_, inner, _) => inner.data,
            _ => self,
        }
    }

    fn wrap_inner(lhs: Vec<Comment>, inner: Box<Located<Self>>, rhs: Vec<Comment>) -> Self {
        Token::Ws(lhs, inner, rhs)
    }
}

impl CanWrapWhitespace for Expression {
    fn strip_whitespace(self) -> Self {
        match self {
            Expression::BinaryAdd(lhs, rhs) => Expression::BinaryAdd(sb(lhs), sb(rhs)),
            Expression::BinarySub(lhs, rhs) => Expression::BinarySub(sb(lhs), sb(rhs)),
            Expression::BinaryMul(lhs, rhs) => Expression::BinaryMul(sb(lhs), sb(rhs)),
            Expression::BinaryDiv(lhs, rhs) => Expression::BinaryDiv(sb(lhs), sb(rhs)),
            Expression::ExprParens(inner) => Expression::ExprParens(sb(inner)),
            Expression::Ws(_, inner, _) => inner.data,
            _ => self,
        }
    }

    fn wrap_inner(lhs: Vec<Comment>, inner: Box<Located<Self>>, rhs: Vec<Comment>) -> Self {
        Expression::Ws(lhs, inner, rhs)
    }
}

impl Display for Mnemonic {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let m = format!("{:?}", self).to_uppercase();
        write!(f, "{}", m)
    }
}

impl Display for Comment {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Comment::CStyle(str) => write!(f, "{}", str),
            Comment::CppStyle(str) => write!(f, "{}", str),
        }
    }
}

impl Display for Expression {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Expression::Identifier(id) => {
                write!(f, "{}", id.0)
            }
            Expression::Number(val, ty) => match ty {
                NumberType::Hex => write!(f, "${:x}", val),
                NumberType::Dec => write!(f, "{}", val),
            },
            Expression::ExprParens(inner) => {
                write!(f, "[{}]", inner.data)
            }
            Expression::BinaryAdd(lhs, rhs) => {
                write!(f, "{} + {}", lhs.data, rhs.data)
            }
            Expression::BinarySub(lhs, rhs) => {
                write!(f, "{} - {}", lhs.data, rhs.data)
            }
            Expression::BinaryMul(lhs, rhs) => {
                write!(f, "{} * {}", lhs.data, rhs.data)
            }
            Expression::BinaryDiv(lhs, rhs) => {
                write!(f, "{} / {}", lhs.data, rhs.data)
            }
            Expression::Ws(l, inner, r) => format_ws(f, l, inner, r),
        }
    }
}

impl Display for Token {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Token::Label(id) => {
                write!(f, "{}:", id.0)
            }
            Token::Instruction(i) => match &i.operand {
                Some(o) => {
                    write!(f, "{}{}", i.mnemonic, o.data)
                }
                None => write!(f, "{}", i.mnemonic),
            },
            Token::Operand(o) => {
                let suffix = match &o.suffix {
                    Some(s) => format!("{}", s.data),
                    None => "".to_string(),
                };

                match &o.addressing_mode {
                    AddressingMode::Immediate => write!(f, " #{}", o.expr.data),
                    AddressingMode::Implied => write!(f, ""),
                    AddressingMode::AbsoluteOrZP => {
                        write!(f, " {}{}", o.expr.data, suffix)
                    }
                    AddressingMode::OuterIndirect => {
                        write!(f, " ({}){}", o.expr.data, suffix)
                    }
                    AddressingMode::Indirect => {
                        write!(f, " ({}{})", o.expr.data, suffix)
                    }
                }
            }
            Token::RegisterSuffix(reg) => match reg {
                Register::X => write!(f, ", x"),
                Register::Y => write!(f, ", y"),
            },
            Token::Ws(l, inner, r) => format_ws(f, l, inner, r),
            Token::Data(tok, sz) => {
                let label = match sz {
                    1 => ".byte",
                    2 => ".word",
                    4 => ".dword",
                    _ => panic!(),
                };
                match tok {
                    Some(t) => write!(f, "{} {}", label, t.data),
                    None => write!(f, "{}", label),
                }
            }
            Token::Error => write!(f, "Error"),
        }
    }
}

fn format_ws<T: CanWrapWhitespace + Display>(
    f: &mut Formatter,
    l: &[Comment],
    inner: &Located<T>,
    r: &[Comment],
) -> std::fmt::Result {
    for w in l {
        let _ = write!(f, "{}", w);
    }
    let _ = write!(f, "{}", inner.data);
    for w in r {
        let _ = write!(f, "{}", w);
    }
    Ok(())
}
