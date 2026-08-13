use crate::{
    generator::{Generator, default::default_format_type, utils::split_name},
    parser::types::{EnumCase, EnumSource},
};

pub trait EnumCaseGenerator {
    fn generate(&self, entity: &EnumCase, generator: &Generator) -> String {
        let typename = if entity.type_name.is_some() {
            format!("({})", self.get_type_name(entity, generator))
        } else {
            "".into()
        };
        format!(
            "{comment}{macros}{indent}{name}{typename},",
            indent = generator.base().indent(),
            name = self.get_name(entity, generator),
            comment = self.format_comment(entity, generator),
            macros = self.macros(entity, generator),
            typename = typename
        )
    }

    fn get_name(&self, entity: &EnumCase, generator: &Generator) -> String {
        default_format_type(entity.name.as_str(), &generator.target_ns.borrow())
            .split("::")
            .last()
            .unwrap()
            .to_string()
    }

    fn get_type_name(&self, entity: &EnumCase, generator: &Generator) -> String {
        let formatted_type = generator
            .base()
            .format_type_name(entity.type_name.as_ref().unwrap(), generator);
        generator
            .base()
            .modify_type(formatted_type.as_ref(), &entity.type_modifiers)
            .into()
    }

    fn format_comment(&self, entity: &EnumCase, generator: &Generator) -> String {
        generator
            .base()
            .format_comment(entity.comment.as_deref(), generator.base().indent_size())
    }

    fn macros(&self, entity: &EnumCase, generator: &Generator) -> String {
        if entity.source == EnumSource::Union {
            return "".into();
        }

        let (prefix, field_name) = split_name(entity.name.as_str());
        prefix.map_or_else(
            || {
                if field_name == self.get_name(entity, generator) {
                    "".into()
                } else {
                    format!(
                        "{indent}#[yaserde(rename = \"{rename}\")]\n",
                        indent = generator.base().indent(),
                        rename = field_name
                    )
                }
            },
            |prefix| {
                format!(
                    "{indent}#[yaserde(prefix = \"{prefix}\", rename = \"{rename}\")]\n",
                    indent = generator.base().indent(),
                    rename = field_name
                )
            },
        )
    }
}

pub struct DefaultEnumCaseGen;
impl EnumCaseGenerator for DefaultEnumCaseGen {}
