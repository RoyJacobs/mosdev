use crate::errors::AsmError;
use crate::parser::mnemonic::Mnemonic;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};

pub type LocatedSpan<'a> = nom_locate::LocatedSpan<&'a str, State<'a>>;
pub type IResult<'a, T> = nom::IResult<LocatedSpan<'a>, T>;

/*#[derive(Debug)]
pub struct Error {
    pub location: Location,
    pub message: String,
}*/

#[derive(Clone, Debug)]
pub struct State<'a> {
    pub errors: &'a RefCell<Vec<AsmError>>,
}

impl<'a> State<'a> {
    pub fn report_error(&self, error: AsmError) {
        self.errors.borrow_mut().push(error);
    }
}

#[derive(Debug)]
pub enum Comment {
    CStyle(String),
    CppStyle(String),
}

#[derive(Debug)]
pub struct Identifier(pub String);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub line: u32,
    pub column: u32,
}

impl<'a> From<&LocatedSpan<'a>> for Location {
    fn from(span: &LocatedSpan) -> Self {
        Self {
            line: span.location_line(),
            column: span.get_column() as u32,
        }
    }
}

#[derive(Debug)]
pub enum Register {
    A,
    X,
    Y,
}

#[derive(Debug)]
pub enum Token {
    Identifier(Identifier),
    Label(Identifier),
    Mnemonic(Mnemonic),
    Number(usize),
    Instruction((Location, Mnemonic, Option<Box<Token>>)),
    IndirectAddressing((Box<Token>, Option<Register>)),
    Ws((Vec<Comment>, Box<Token>, Vec<Comment>)),
    ExprParens(Box<Token>),
    BinaryAdd(Box<Token>, Box<Token>),
    BinarySub(Box<Token>, Box<Token>),
    BinaryMul(Box<Token>, Box<Token>),
    BinaryDiv(Box<Token>, Box<Token>),
    Data(Option<Box<Token>>, usize),
    Error,
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

impl Display for Token {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Token::Label(id) => {
                write!(f, "{}:", id.0)
            }
            Token::Instruction((_location, mnemonic, operand)) => match operand {
                Some(o) => write!(f, "\t\t{} {}", mnemonic, o),
                None => write!(f, "\t\t{}", mnemonic),
            },
            Token::Identifier(id) => {
                write!(f, "{}", id.0)
            }
            Token::IndirectAddressing((id, reg)) => match reg {
                Some(r) => {
                    let r = format!("{:?}", r).to_lowercase();
                    write!(f, "({}), {}", id, r)
                }
                None => write!(f, "({})", id),
            },
            Token::Ws((l, inner, r)) => {
                for w in l {
                    let _ = write!(f, "{}", w);
                }
                let _ = write!(f, "{}", inner);
                for w in r {
                    let _ = write!(f, "\t\t{}", w);
                }
                Ok(())
            }
            Token::Number(num) => {
                write!(f, "{}", num)
            }
            Token::ExprParens(inner) => {
                write!(f, "[{}]", inner)
            }
            Token::BinaryAdd(lhs, rhs) => {
                write!(f, "{} + {}", lhs, rhs)
            }
            Token::BinarySub(lhs, rhs) => {
                write!(f, "{} - {}", lhs, rhs)
            }
            Token::BinaryMul(lhs, rhs) => {
                write!(f, "{} * {}", lhs, rhs)
            }
            Token::BinaryDiv(lhs, rhs) => {
                write!(f, "{} / {}", lhs, rhs)
            }
            Token::Data(tok, sz) => {
                let label = match sz {
                    8 => ".byte",
                    16 => ".word",
                    32 => ".dword",
                    _ => panic!(),
                };
                match tok {
                    Some(t) => write!(f, "{} {}", label, t),
                    None => write!(f, "{}", label),
                }
            }
            _ => Ok(()),
        }
    }
}
