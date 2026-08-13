use crate::{
    generator::{
        Generator,
        default::{yaserde_for_attribute, yaserde_for_element, yaserde_for_flatten_element},
    },
    parser::types::{StructField, StructFieldSource, TypeModifier},
};

pub trait StructFieldGenerator {
    fn generate(&self, entity: &StructField, generator: &Generator) -> String {
        if entity.type_modifiers.contains(&TypeModifier::Empty) {
            return "".into();
        }
        format!(
            "{comment}{macros}{indent}pub {name}: {typename},",
            comment = self.format_comment(entity, generator),
            macros = self.macros(entity, generator),
            indent = generator.base().indent(),
            name = self.get_name(entity, generator),
            typename = self.get_type_name(entity, generator),
        )
    }

    fn get_type_name(&self, entity: &StructField, generator: &Generator) -> String {
        generator
            .base()
            .modify_type(
                generator
                    .base()
                    .format_type_name(entity.type_name.as_str(), generator)
                    .as_ref(),
                &entity.type_modifiers,
            )
            .into()
    }

    fn get_name(&self, entity: &StructField, generator: &Generator) -> String {
        generator.base().format_name(entity.name.as_str()).into()
    }

    fn format_comment(&self, entity: &StructField, generator: &Generator) -> String {
        generator
            .base()
            .format_comment(entity.comment.as_deref(), generator.base().indent_size())
    }

    fn macros(&self, entity: &StructField, generator: &Generator) -> String {
        let indent = generator.base().indent();
        match entity.source {
            StructFieldSource::Choice => yaserde_for_flatten_element(indent.as_str()),
            StructFieldSource::Attribute => {
                yaserde_for_attribute(entity.name.as_str(), indent.as_str())
            }
            StructFieldSource::Element => yaserde_for_element(
                entity.name.as_str(),
                generator.target_ns.borrow().as_ref(),
                indent.as_str(),
            ),
            _ => "".into(),
        }
    }
}

pub struct DefaultStructFieldGen;
impl StructFieldGenerator for DefaultStructFieldGen {}
