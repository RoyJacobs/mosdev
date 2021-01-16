use crate::parser::Location;

pub type AsmResult<T> = Result<T, AsmError>;

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum AsmError {
    #[error("parser error")]
    Parser { location: Location, message: String },
    #[error("unknown error")]
    Unknown,
}
