#![allow(dead_code)]
#![allow(unused_imports)]
use std::cell::RefCell;
use std::ops::Range;

use crate::parser2::mnemonic::{mnemonic, Mnemonic};
use nom::branch::alt;
use nom::bytes::complete::{is_not, tag, tag_no_case, take, take_till1, take_until, take_while};
use nom::character::complete::{
    alpha1, alphanumeric1, anychar, char, digit1, hex_digit1, multispace1, newline, space1,
};
use nom::combinator::{
    all_consuming, map, map_opt, map_res, not, opt, recognize, rest, value, verify,
};
use nom::lib::std::fmt::Formatter;
use nom::multi::{many0, many1};
use nom::sequence::{delimited, pair, preceded, terminated, tuple};
use std::fmt::Display;

mod mnemonic;

type LocatedSpan<'a> = nom_locate::LocatedSpan<&'a str, State<'a>>;
type IResult<'a, T> = nom::IResult<LocatedSpan<'a>, T>;

#[derive(Debug)]
struct Error(Location, String);

#[derive(Clone, Debug)]
struct State<'a> {
    errors: &'a RefCell<Vec<Error>>,
}

impl<'a> State<'a> {
    pub fn report_error(&self, error: Error) {
        self.errors.borrow_mut().push(error);
    }
}

fn expect<'a, F, E, T>(
    mut parser: F,
    error_msg: E,
) -> impl FnMut(LocatedSpan<'a>) -> IResult<Option<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
    E: ToString,
{
    move |input| {
        let i = input.clone();
        match parser(input) {
            Ok((remaining, out)) => Ok((remaining, Some(out))),
            Err(nom::Err::Error(_)) | Err(nom::Err::Failure(_)) => {
                let err = Error(Location::from(&i), error_msg.to_string());
                i.extra.report_error(err);
                Ok((i, None))
            }
            Err(err) => Err(err),
        }
    }
}

fn cpp_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(pair(tag("//"), is_not("\n\r")))(input)
}

fn inside_c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    // Once we're inside a C comment, we don't care about anything except perhaps another /*
    let (input, _) = take_until("/*")(input)?;

    // Found another /*, so let's consume it
    let (input, _) = tag("/*")(input)?;

    // Found another /*, so now we either recurse or we go on until we're at the closing */
    let (input, _) = expect(
        pair(alt((inside_c_comment, take_until("*/"))), tag("*/")),
        "missing closing */",
    )(input)?;

    // Ignore any trailing characters until we're up to the next (one level up) */, so the outer function can deal with that
    take_until("*/")(input)
}

fn c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(tuple((
        tag("/*"),
        expect(
            tuple((alt((inside_c_comment, take_until("*/"))), tag("*/"))),
            "missing closing */",
        ),
    )))(input)
}

#[derive(Debug)]
enum Comment {
    CStyle(String),
    CppStyle(String),
}

fn whitespace_or_comment<'a>() -> impl FnMut(LocatedSpan<'a>) -> IResult<Vec<Comment>> {
    map(
        many0(alt((
            map(space1, |_| None),
            map(c_comment, |span| {
                Some(Comment::CStyle(span.fragment().clone().into()))
            }),
            map(cpp_comment, |span| {
                Some(Comment::CppStyle(span.fragment().clone().into()))
            }),
        ))),
        |comments| comments.into_iter().flatten().collect::<Vec<_>>(),
    )
}

fn ws<'a, F>(inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Token>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<Token>,
{
    map(
        tuple((whitespace_or_comment(), inner, whitespace_or_comment())),
        |(l, i, r)| {
            if l.is_empty() && r.is_empty() {
                i
            } else {
                Token::Ws((l, Box::new(i), r))
            }
        },
    )
}

#[derive(Debug)]
struct Identifier(String);

#[derive(Clone, Copy, Debug)]
struct Location {
    line: u32,
    column: u32,
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
enum Register {
    A,
    X,
    Y,
}

#[derive(Debug)]
enum Token {
    Identifier(Identifier),
    Label(Identifier),
    Mnemonic(Mnemonic),
    Number(usize),
    Instruction((Location, Mnemonic, Option<Box<Token>>)),
    IndirectAddressing((Box<Token>, Option<Register>)),
    Ws((Vec<Comment>, Box<Token>, Vec<Comment>)),
    Error,
}

fn identifier(input: LocatedSpan) -> IResult<Token> {
    let id = recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ));

    map(id, |span: LocatedSpan| {
        Token::Identifier(Identifier(span.fragment().to_string()))
    })(input)
}

fn addressing_mode<'a, F>(inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Token>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<Token>,
{
    let inner_with_parens = delimited(
        char('('),
        expect(inner, "expected expression after `(`"),
        expect(char(')'), "missing `)`"),
    );

    let am = tuple((
        inner_with_parens,
        opt(expect(
            alt((
                map(tuple((char(','), tag_no_case("x"))), |_| Some(Register::X)),
                map(tuple((char(','), tag_no_case("y"))), |_| Some(Register::Y)),
                map(tuple((char(','), alpha1)), |(_, i)| {
                    let err = Error(Location::from(&i), format!("invalid register: {}", i));
                    i.extra.report_error(err);
                    None
                }),
            )),
            "expected register X or Y",
        )),
    ));

    map(am, |(inner, register)| {
        Token::IndirectAddressing((
            Box::new(inner.unwrap_or(Token::Error)),
            register.flatten().flatten(),
        ))
    })
}

enum Expression {
    Identifier(Identifier),
    Number(usize),
}

fn operand(input: LocatedSpan) -> IResult<Token> {
    alt((identifier, number))(input)
}

fn instruction(input: LocatedSpan) -> IResult<Token> {
    let location = Location::from(&input);

    let instruction = tuple((
        mnemonic,
        expect(
            opt(alt((ws(operand), ws(addressing_mode(ws(operand)))))),
            "expected single operand after opcode",
        ),
    ));

    map(instruction, move |(mnemonic, operand)| {
        let operand = operand.flatten().map(|operand| Box::new(operand));
        Token::Instruction((location, mnemonic, operand))
    })(input)
}

fn error(input: LocatedSpan) -> IResult<Token> {
    map(
        take_till1(|c| c == ')' || c == '\n' || c == '\r'),
        |span: LocatedSpan| {
            let err = Error(
                Location::from(&span),
                format!("unexpected `{}`", span.fragment()),
            );
            span.extra.report_error(err);
            Token::Error
        },
    )(input)
}

fn label(input: LocatedSpan) -> IResult<Token> {
    map(
        tuple((identifier, expect(char(':'), "labels should end with ':'"))),
        |(id, _)| match id {
            Token::Identifier(id) => Token::Label(id),
            _ => panic!(),
        },
    )(input)
}

fn expr(input: LocatedSpan) -> IResult<Token> {
    alt((instruction, label, error))(input)
}

fn source_file(input: LocatedSpan) -> IResult<Vec<Token>> {
    terminated(
        many0(map(pair(ws(expr), many0(newline)), |(expr, _)| expr)),
        preceded(expect(not(anychar), "expected EOF"), rest),
    )(input)
}

fn number(input: LocatedSpan) -> IResult<Token> {
    map(
        alt((
            preceded(
                char('$'),
                map_opt(expect(hex_digit1, "expected hexadecimal value"), |input| {
                    input.map(|i| usize::from_str_radix(i.fragment(), 16).ok())
                }),
            ),
            map_opt(digit1, |input: LocatedSpan| {
                Some(usize::from_str_radix(input.fragment(), 10).ok())
            }),
        )),
        |val| Token::Number(val.unwrap()),
    )(input)
}

fn parse(source: &str) -> (Vec<Token>, Vec<Error>) {
    let errors = RefCell::new(Vec::new());
    let input = LocatedSpan::new_extra(source, State { errors: &errors });
    let (_, expr) = all_consuming(source_file)(input).expect("parser cannot fail");
    (expr, errors.into_inner())
}

impl Display for Mnemonic {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let mnem = format!("{:?}", self).to_uppercase();
        write!(f, "{}", mnem)
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
                Some(o) => write!(f, "{} {}", mnemonic, o),
                None => write!(f, "{}", mnemonic),
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
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::parser2::parse;

    #[test]
    fn test() {
        println!("{:?}", parse("label: lda (foo),x\nsta $fc"));
    }

    #[test]
    fn pretty_print() {
        for token in parse("label: lda (foo),x/* test */\nsta bar // some comment\nbrk").0 {
            println!("\t{}", token);
        }
    }
}
