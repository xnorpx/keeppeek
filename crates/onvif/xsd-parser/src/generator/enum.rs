use crate::{
    generator::{Generator, validator::gen_validate_impl},
    parser::types::{Enum, EnumSource},
};
use std::borrow::Cow;

pub trait EnumGenerator {
    fn generate(&self, entity: &Enum, generator: &Generator) -> String {
        let name = self.get_name(entity, generator);
        let default_case = format!(
            "impl Default for {name} {{\n\
            {indent}fn default() -> Self {{\n\
            {indent}{indent}Self::__Unknown__(\"No valid variants\".into())\n\
            {indent}}}\n\
            }}",
            name = name,
            indent = generator.base().indent()
        );

        format!(
            "{comment}{macros}\n\
            pub enum {name} {{\n\
                {cases}\n\
                {indent}__Unknown__({typename}),\n\
            }}\n\n\
            {default}\n\n\
            {validation}\n\n\
            {subtypes}\n\n",
            indent = generator.base().indent(),
            comment = self.format_comment(entity, generator),
            macros = self.macros(entity, generator),
            name = name,
            cases = self.cases(entity, generator),
            typename = self.get_type_name(entity, generator),
            default = default_case,
            subtypes = self.subtypes(entity, generator),
            validation = self.validation(entity, generator),
        )
    }

    fn cases(&self, entity: &Enum, generator: &Generator) -> String {
        entity
            .cases
            .iter()
            .map(|case| generator.enum_case_gen().generate(case, generator))
            .collect::<Vec<String>>()
            .join("\n")
    }

    fn subtypes(&self, entity: &Enum, generator: &Generator) -> String {
        generator
            .base()
            .join_subtypes(entity.subtypes.as_ref(), generator)
    }

    fn get_type_name(&self, entity: &Enum, generator: &Generator) -> String {
        generator
            .base()
            .format_type_name(entity.type_name.as_str(), generator)
            .into()
    }

    fn get_name(&self, entity: &Enum, generator: &Generator) -> String {
        generator
            .base()
            .format_type_name(entity.name.as_str(), generator)
            .into()
    }

    fn macros(&self, entity: &Enum, generator: &Generator) -> Cow<'static, str> {
        if entity.source == EnumSource::Union {
            return "#[derive(PartialEq, Debug, UtilsUnionSerDe)]".into();
        }

        let derives = "#[derive(PartialEq, Debug, Clone, YaSerialize, YaDeserialize)]";
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

    fn format_comment(&self, entity: &Enum, generator: &Generator) -> String {
        generator
            .base()
            .format_comment(entity.comment.as_deref(), 0)
    }

    fn validation(&self, entity: &Enum, generator: &Generator) -> Cow<'static, str> {
        // Empty validation
        Cow::Owned(gen_validate_impl(
            self.get_name(entity, generator).as_str(),
            "",
        ))
    }
}

pub struct DefaultEnumGen;
impl EnumGenerator for DefaultEnumGen {}
