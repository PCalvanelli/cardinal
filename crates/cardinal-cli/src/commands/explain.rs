use super::{EXIT_TOOL, eprint_error};
use cardinal_diag::Code;

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
        Code::G006 => page!("G006.md"),
        Code::G007 => page!("G007.md"),
        Code::G008 => page!("G008.md"),
        Code::G009 => page!("G009.md"),
        Code::G010 => page!("G010.md"),
        Code::G011 => page!("G011.md"),
        Code::G012 => page!("G012.md"),
        Code::G013 => page!("G013.md"),
        Code::G014 => page!("G014.md"),
        Code::P001 => page!("P001.md"),
        Code::P002 => page!("P002.md"),
        Code::P003 => page!("P003.md"),
        Code::D001 => page!("D001.md"),
        Code::D002 => page!("D002.md"),
        Code::E001 => page!("E001.md"),
        Code::E002 => page!("E002.md"),
        Code::E003 => page!("E003.md"),
        Code::E004 => page!("E004.md"),
        Code::E005 => page!("E005.md"),
        Code::E006 => page!("E006.md"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use cardinal_diag::ALL_CODES;

    #[test]
    fn every_page_names_its_layer() {
        for c in ALL_CODES {
            let line = page(*c)
                .lines()
                .find(|l| l.starts_with("Layer: "))
                .unwrap_or_else(|| panic!("{c}: no Layer: line"));
            let expected = format!("Layer: **{}**", c.layer());
            assert!(line.starts_with(&expected), "{c}: {line}");
        }
    }
}
