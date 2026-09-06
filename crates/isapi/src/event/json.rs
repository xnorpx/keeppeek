use serde::Deserialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::collections::HashSet;
use std::fmt;

use crate::Error;
use crate::error::Kind;

#[derive(Deserialize)]
#[serde(untagged)]
enum Number {
    Unsigned(u32),
    Text(String),
}

impl Number {
    fn text(self) -> String {
        match self {
            Self::Unsigned(value) => value.to_string(),
            Self::Text(value) => value,
        }
    }
}

#[derive(Deserialize)]
struct Fields {
    #[serde(rename = "eventType")]
    kind: Option<String>,
    #[serde(rename = "eventState")]
    state: Option<String>,
    #[serde(rename = "channelID")]
    channel: Option<Number>,
    #[serde(rename = "dynChannelID")]
    dynamic_channel: Option<Number>,
    #[serde(rename = "dateTime")]
    timestamp: Option<String>,
    #[serde(rename = "activePostCount")]
    count: Option<Number>,
    #[serde(rename = "detectionTarget")]
    target: Option<String>,
    #[serde(rename = "channelName")]
    name: Option<String>,
    #[serde(rename = "EventNotificationAlert")]
    wrapped: Option<Box<Self>>,
}

pub fn parse(bytes: &[u8]) -> Result<[Option<String>; 8], Error> {
    validate(bytes)?;
    fields(bytes)
}

pub fn validate(bytes: &[u8]) -> Result<(), Error> {
    let mut count = 0;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Shape {
        depth: 1,
        count: &mut count,
    }
    .deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(())
}

fn fields(bytes: &[u8]) -> Result<[Option<String>; 8], Error> {
    let mut fields: Fields = serde_json::from_slice(bytes)?;
    if let Some(wrapped) = fields.wrapped.take() {
        if fields.kind.is_some()
            || fields.state.is_some()
            || fields.channel.is_some()
            || fields.dynamic_channel.is_some()
            || fields.timestamp.is_some()
            || fields.count.is_some()
            || fields.target.is_some()
            || fields.name.is_some()
            || wrapped.wrapped.is_some()
        {
            return Err(Error::new(Kind::Protocol));
        }
        fields = *wrapped;
    }
    Ok([
        fields.kind,
        fields.state,
        fields.channel.map(Number::text),
        fields.dynamic_channel.map(Number::text),
        fields.timestamp,
        fields.count.map(Number::text),
        fields.target,
        fields.name,
    ])
}

struct Shape<'budget> {
    depth: usize,
    count: &'budget mut usize,
}

impl<'de> DeserializeSeed<'de> for Shape<'_> {
    type Value = ();

    fn deserialize<Deserializer: serde::Deserializer<'de>>(
        self,
        decoder: Deserializer,
    ) -> Result<(), Deserializer::Error> {
        *self.count += 1;
        if self.depth > super::DEPTH_MAX || *self.count > super::ELEMENT_COUNT_MAX {
            return Err(serde::de::Error::custom(
                "ISAPI JSON structure limit exceeded",
            ));
        }
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON with unique object keys")
    }

    fn visit_map<Access: MapAccess<'de>>(self, mut access: Access) -> Result<(), Access::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = access.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate ISAPI JSON field"));
            }
            access.next_value_seed(Shape {
                depth: self.depth + 1,
                count: self.count,
            })?;
        }
        Ok(())
    }

    fn visit_seq<Access: SeqAccess<'de>>(self, mut access: Access) -> Result<(), Access::Error> {
        while access
            .next_element_seed(Shape {
                depth: self.depth + 1,
                count: self.count,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_bool<ErrorType>(self, _: bool) -> Result<(), ErrorType> {
        Ok(())
    }
    fn visit_i64<ErrorType>(self, _: i64) -> Result<(), ErrorType> {
        Ok(())
    }
    fn visit_u64<ErrorType>(self, _: u64) -> Result<(), ErrorType> {
        Ok(())
    }
    fn visit_f64<ErrorType>(self, _: f64) -> Result<(), ErrorType> {
        Ok(())
    }
    fn visit_str<ErrorType>(self, _: &str) -> Result<(), ErrorType> {
        Ok(())
    }
    fn visit_unit<ErrorType>(self) -> Result<(), ErrorType> {
        Ok(())
    }
}
