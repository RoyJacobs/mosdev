use super::{Mnemonic, LocatedMnemonic, Span};
use nom::IResult;
use nom::branch::alt;
use nom::combinator::map;
use nom::bytes::complete::tag_no_case;
use nom_locate::position;

macro_rules! parse_mnemonic {
    ( $ input : expr , $ expected : expr ) => {
        map(tag_no_case($input), |_| $expected)
    };
}

pub(super) fn parse_mnemonic(input: Span) -> IResult<Span, LocatedMnemonic> {
    let (input, position) = position(input)?;

    let (input, data) = alt((
        alt((
            parse_mnemonic!("adc", Mnemonic::Adc),
            parse_mnemonic!("and", Mnemonic::And),
            parse_mnemonic!("asl", Mnemonic::Asl),
            parse_mnemonic!("bcc", Mnemonic::Bcc),
            parse_mnemonic!("bcs", Mnemonic::Bcs),
            parse_mnemonic!("beq", Mnemonic::Beq),
            parse_mnemonic!("bit", Mnemonic::Bit),
            parse_mnemonic!("bmi", Mnemonic::Bmi),
            parse_mnemonic!("bne", Mnemonic::Bne),
            parse_mnemonic!("bpl", Mnemonic::Bpl),
            parse_mnemonic!("brk", Mnemonic::Brk),
            parse_mnemonic!("bvc", Mnemonic::Bvc),
            parse_mnemonic!("bvs", Mnemonic::Bvs),
            parse_mnemonic!("clc", Mnemonic::Clc),
            parse_mnemonic!("cld", Mnemonic::Cld),
            parse_mnemonic!("cli", Mnemonic::Cli),
            parse_mnemonic!("clv", Mnemonic::Clv),
            parse_mnemonic!("cmp", Mnemonic::Cmp),
            parse_mnemonic!("cpx", Mnemonic::Cpx),
            parse_mnemonic!("cpy", Mnemonic::Cpy),
            parse_mnemonic!("dec", Mnemonic::Dec),
        )),
        alt((
            parse_mnemonic!("dex", Mnemonic::Dex),
            parse_mnemonic!("dey", Mnemonic::Dey),
            parse_mnemonic!("eor", Mnemonic::Eor),
            parse_mnemonic!("inc", Mnemonic::Inc),
            parse_mnemonic!("inx", Mnemonic::Inx),
            parse_mnemonic!("iny", Mnemonic::Iny),
            parse_mnemonic!("jmp", Mnemonic::Jmp),
            parse_mnemonic!("jsr", Mnemonic::Jsr),
            parse_mnemonic!("lda", Mnemonic::Lda),
            parse_mnemonic!("ldx", Mnemonic::Ldx),
            parse_mnemonic!("ldy", Mnemonic::Ldy),
            parse_mnemonic!("lsr", Mnemonic::Lsr),
            parse_mnemonic!("nop", Mnemonic::Nop),
            parse_mnemonic!("ora", Mnemonic::Ora),
            parse_mnemonic!("pha", Mnemonic::Pha),
            parse_mnemonic!("php", Mnemonic::Php),
            parse_mnemonic!("pla", Mnemonic::Pla),
            parse_mnemonic!("plp", Mnemonic::Plp),
            parse_mnemonic!("rol", Mnemonic::Rol),
            parse_mnemonic!("ror", Mnemonic::Ror),
            parse_mnemonic!("rti", Mnemonic::Rti),
        )),
        alt((
            parse_mnemonic!("rts", Mnemonic::Rts),
            parse_mnemonic!("sbc", Mnemonic::Sbc),
            parse_mnemonic!("sec", Mnemonic::Sec),
            parse_mnemonic!("sed", Mnemonic::Sed),
            parse_mnemonic!("sei", Mnemonic::Sei),
            parse_mnemonic!("sta", Mnemonic::Sta),
            parse_mnemonic!("stx", Mnemonic::Stx),
            parse_mnemonic!("sty", Mnemonic::Sty),
            parse_mnemonic!("tax", Mnemonic::Tax),
            parse_mnemonic!("tay", Mnemonic::Tay),
            parse_mnemonic!("tsx", Mnemonic::Tsx),
            parse_mnemonic!("txa", Mnemonic::Txa),
            parse_mnemonic!("txs", Mnemonic::Txs),
            parse_mnemonic!("tya", Mnemonic::Tya),
        ))
    ))(input)?;

    Ok((input, LocatedMnemonic { position, data }))
}