use std::{error::Error as StdError, fmt};

const FRAGMENT_SIZE: usize = u8::MAX as usize;
const LIST_SEPARATOR: u8 = 0x00;
const PAIRING_SEPARATOR: u8 = 0xff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnexpectedEnd,
    ValueTooLarge { maximum: usize },
    InvalidIntegerWidth { expected: usize, actual: usize },
    DuplicateType(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("TLV8 item extends beyond the input"),
            Self::ValueTooLarge { maximum } => {
                write!(f, "TLV8 logical value exceeds {maximum} bytes")
            }
            Self::InvalidIntegerWidth { expected, actual } => {
                write!(f, "TLV8 integer has {actual} bytes; expected {expected}")
            }
            Self::DuplicateType(item_type) => {
                write!(f, "TLV8 type {item_type} occurs more than once")
            }
        }
    }
}

impl StdError for Error {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv8Map {
    items: Vec<(u8, Vec<u8>)>,
}

impl Tlv8Map {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse_bounded(bytes, usize::MAX)
    }

    pub fn parse_bounded(bytes: &[u8], maximum_value_size: usize) -> Result<Self, Error> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut open_fragment_type = None;
        let mut offset = 0;
        while offset < bytes.len() {
            let item_type = bytes[offset];
            let length = *bytes.get(offset + 1).ok_or(Error::UnexpectedEnd)? as usize;
            let value_start = offset + 2;
            let value_end = value_start
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or(Error::UnexpectedEnd)?;
            let value = &bytes[value_start..value_end];
            offset = value_end;

            let continues = open_fragment_type == Some(item_type);
            if continues {
                let existing = &mut items.last_mut().expect("open fragment has an item").1;
                let combined = existing
                    .len()
                    .checked_add(value.len())
                    .filter(|length| *length <= maximum_value_size)
                    .ok_or(Error::ValueTooLarge {
                        maximum: maximum_value_size,
                    })?;
                existing.reserve(combined - existing.len());
                existing.extend_from_slice(value);
            } else if length == 0 {
                items.push((item_type, Vec::new()));
            } else {
                if value.len() > maximum_value_size {
                    return Err(Error::ValueTooLarge {
                        maximum: maximum_value_size,
                    });
                }
                items.push((item_type, value.to_vec()));
            }
            open_fragment_type = (length == FRAGMENT_SIZE).then_some(item_type);
        }
        Ok(Self { items })
    }

    pub fn items(&self) -> &[(u8, Vec<u8>)] {
        &self.items
    }

    pub fn get_unique(&self, item_type: u8) -> Result<Option<&[u8]>, Error> {
        let mut values = self
            .items
            .iter()
            .filter(|(candidate, _)| *candidate == item_type)
            .map(|(_, value)| value.as_slice());
        let first = values.next();
        if values.next().is_some() {
            return Err(Error::DuplicateType(item_type));
        }
        Ok(first)
    }

    pub fn get_u8(&self, item_type: u8) -> Result<Option<u8>, Error> {
        let Some(value) = self.get_unique(item_type)? else {
            return Ok(None);
        };
        let [value] = value else {
            return Err(Error::InvalidIntegerWidth {
                expected: 1,
                actual: value.len(),
            });
        };
        Ok(Some(*value))
    }
}

pub struct Tlv8Writer<'a> {
    output: &'a mut Vec<u8>,
}

impl<'a> Tlv8Writer<'a> {
    pub const fn new(output: &'a mut Vec<u8>) -> Self {
        Self { output }
    }

    pub fn push(&mut self, item_type: u8, value: &[u8]) {
        if value.is_empty() {
            self.output.extend_from_slice(&[item_type, 0]);
            return;
        }

        let mut final_length = 0;
        for fragment in value.chunks(FRAGMENT_SIZE) {
            self.output.push(item_type);
            self.output.push(fragment.len() as u8);
            self.output.extend_from_slice(fragment);
            final_length = fragment.len();
        }
        if final_length == FRAGMENT_SIZE {
            self.output.extend_from_slice(&[item_type, 0]);
        }
    }

    pub fn push_u8(&mut self, item_type: u8, value: u8) {
        self.push(item_type, &[value]);
    }

    pub fn push_u16(&mut self, item_type: u8, value: u16) {
        self.push(item_type, &value.to_le_bytes());
    }

    pub fn push_u32(&mut self, item_type: u8, value: u32) {
        self.push(item_type, &value.to_le_bytes());
    }

    pub fn push_str(&mut self, item_type: u8, value: &str) {
        self.push(item_type, value.as_bytes());
    }

    pub fn push_separator(&mut self) {
        self.push(PAIRING_SEPARATOR, &[]);
    }

    pub fn push_list_separator(&mut self) {
        self.push(LIST_SEPARATOR, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_and_reassembles_values() {
        let value = vec![0x5a; 510];
        let mut encoded = Vec::new();
        Tlv8Writer::new(&mut encoded).push(7, &value);

        let parsed = Tlv8Map::parse_bounded(&encoded, 510).unwrap();
        assert_eq!(parsed.items(), &[(7, value)]);
        assert_eq!(&encoded[0..2], &[7, 255]);
        assert_eq!(&encoded[257..259], &[7, 255]);
        assert_eq!(&encoded[514..516], &[7, 0]);
    }

    #[test]
    fn preserves_repeated_short_items() {
        let parsed = Tlv8Map::parse(&[1, 1, 10, 1, 1, 20]).unwrap();
        assert_eq!(parsed.items(), &[(1, vec![10]), (1, vec![20])]);
        assert_eq!(parsed.get_unique(1), Err(Error::DuplicateType(1)));
    }

    #[test]
    fn writes_distinct_list_and_pairing_separators() {
        let mut encoded = Vec::new();
        let mut writer = Tlv8Writer::new(&mut encoded);
        writer.push_list_separator();
        writer.push_separator();

        assert_eq!(encoded, [0x00, 0x00, 0xff, 0x00]);
        assert_eq!(
            Tlv8Map::parse(&encoded).unwrap().items(),
            &[(0x00, Vec::new()), (0xff, Vec::new())]
        );
    }

    #[test]
    fn accepts_any_zero_length_list_separator() {
        for separator in [0x00, 0x05, 0xff] {
            let encoded = [1, 1, 10, separator, 0, 1, 1, 20];
            let parsed = Tlv8Map::parse(&encoded).unwrap();

            assert_eq!(
                parsed.items(),
                &[(1, vec![10]), (separator, Vec::new()), (1, vec![20])]
            );
        }
    }

    #[test]
    fn rejects_truncated_item() {
        assert_eq!(Tlv8Map::parse(&[1, 2, 10]), Err(Error::UnexpectedEnd));
    }

    #[test]
    fn bounds_fragment_reassembly() {
        let value = vec![0x5a; 256];
        let mut encoded = Vec::new();
        Tlv8Writer::new(&mut encoded).push(7, &value);

        assert_eq!(
            Tlv8Map::parse_bounded(&encoded, 255),
            Err(Error::ValueTooLarge { maximum: 255 })
        );
    }
}
