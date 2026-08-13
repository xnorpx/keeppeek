use crate::{
    generator::{Generator, validator::gen_validate_impl},
    parser::types::Struct,
};
use std::borrow::Cow;

pub trait StructGenerator {
    fn generate(&self, entity: &Struct, generator: &Generator) -> String {
        format!(
            "{comment}{macros}pub struct {name} {{{fields}}}\n\n{validation}\n{subtypes}\n",
            comment = self.format_comment(entity, generator),
            macros = self.macros(entity, generator),
            name = self.get_type_name(entity, generator),
            fields = self.fields(entity, generator),
            subtypes = self.subtypes(entity, generator),
            validation = self.validation(entity, generator),
        )
    }

    fn fields(&self, entity: &Struct, generator: &Generator) -> String {
        let mod_name = self.mod_name(entity, generator);

        entity.fields.borrow_mut().iter_mut().for_each(|f| {
            if !f.subtypes.is_empty() {
                f.type_name = format!("{}::{}", mod_name, f.type_name);
            }
        });

        let fields = entity
            .fields
            .borrow()
            .iter()
            .map(|f| generator.struct_field_gen().generate(f, generator))
            .filter(|s| !s.is_empty())
            .collect::<Vec<String>>()
            .join("\n\n");

        if fields.is_empty() {
            fields
        } else {
            format!("\n{fields}\n")
        }
    }

    fn subtypes(&self, entity: &Struct, generator: &Generator) -> String {
        let field_subtypes = entity
            .fields
            .borrow()
            .iter()
            .map(|f| {
                generator
                    .base()
                    .join_subtypes(f.subtypes.as_ref(), generator)
            })
            .collect::<Vec<String>>()
            .join("");

        let subtypes = generator
            .base()
            .join_subtypes(entity.subtypes.as_ref(), generator);

        if !field_subtypes.is_empty() || !subtypes.is_empty() {
            format!(
                "\npub mod {name} {{\n{indent}use super::*;{st}\n{fst}\n}}\n",
                name = self.mod_name(entity, generator),
                st = subtypes,
                indent = generator.base().indent(),
                fst = self.shift(&field_subtypes, generator.base().indent().as_str())
            )
        } else {
            format!("{subtypes}\n{field_subtypes}")
        }
    }

    fn shift(&self, text: &str, indent: &str) -> String {
        text.replace("\n\n\n", "\n") // TODO: fix this workaround replace
            .split('\n')
            .map(|s| {
                if !s.is_empty() {
                    format!("\n{indent}{s}")
                } else {
                    "\n".to_string()
                }
            })
            .fold(indent.to_string(), |acc, x| acc + &x)
    }

    fn get_type_name(&self, entity: &Struct, generator: &Generator) -> String {
        generator
            .base()
            .format_type_name(entity.name.as_str(), generator)
            .into()
    }

    fn macros(&self, _entity: &Struct, generator: &Generator) -> Cow<'static, str> {
        let derives = "#[derive(Default, Clone, PartialEq, Debug, YaSerialize, YaDeserialize)]\n";
        let tns = generator.target_ns.borrow();
        tns.as_ref()
            .map_or_else(
                || format!("{derives}#[yaserde()]\n"),
                |namespace| {
                    namespace.name().map_or_else(
                        || {
                            format!(
                                "{derives}#[yaserde(namespaces = {{ \"\" = \"{uri}\" }})]\n",
                                uri = namespace.uri()
                            )
                        },
                        |prefix| {
                            format!(
                                "{derives}#[yaserde(prefix = \"{prefix}\", namespaces = {{ \"{prefix}\" = \"{uri}\" }})]\n",
                                uri = namespace.uri()
                            )
                        },
                    )
                },
            )
        .into()
    }

    fn format_comment(&self, entity: &Struct, generator: &Generator) -> String {
        generator
            .base()
            .format_comment(entity.comment.as_deref(), 0)
    }

    fn mod_name(&self, entity: &Struct, generator: &Generator) -> String {
        generator.base().mod_name(entity.name.as_str())
    }

    fn validation(&self, entity: &Struct, generator: &Generator) -> Cow<'static, str> {
        // Empty validation
        Cow::Owned(gen_validate_impl(
            self.get_type_name(entity, generator).as_str(),
            "",
        ))
    }
}

pub struct DefaultStructGen;
impl StructGenerator for DefaultStructGen {}
