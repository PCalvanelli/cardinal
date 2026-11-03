use super::{EXIT_TOOL, eprint_error};
use derive_diag::Code;

macro_rules! page {
    ($f:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/explain/",
            $f
        ))
    };
}

pub fn page(code: Code) -> &'static str {
    match code {
        Code::G001 => page!("G001.md"),
        Code::G002 => page!("G002.md"),
        Code::G003 => page!("G003.md"),
        Code::G004 => page!("G004.md"),
        Code::G005 => page!("G005.md"),
        Code::E001 => page!("E001.md"),
        Code::E002 => page!("E002.md"),
        Code::E003 => page!("E003.md"),
        Code::E004 => page!("E004.md"),
        Code::E005 => page!("E005.md"),
    }
}

pub fn run(code: &str) -> u8 {
    match code.parse::<Code>() {
        Ok(c) => {
            print!("{}", page(c));
            0
        }
        Err(e) => {
            eprint_error(&e);
            EXIT_TOOL
        }
    }
}
