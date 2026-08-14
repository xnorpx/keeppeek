use crate::{
    generator::{
        Generator,
        validator::{gen_facet_validation, gen_validate_impl},
    },
    parser::types::TupleStruct,
};
use std::borrow::Cow;

pub trait TupleStructGenerator {
    fn generate(&self, entity: &TupleStruct, generator: &Generator) -> String {
        format!(
            "{comment}{macros}pub struct {name} (pub {typename});\n{subtypes}\n{validation}\n",
            comment = self.format_comment(entity, generator),
            name = self.get_name(entity, generator),
            macros = self.macros(entity, generator),
            typename = self.get_type_name(entity, generator),
            subtypes = self.subtypes(entity, generator),
            validation = self.validation(entity, generator),
        )
    }

    fn subtypes(&self, entity: &TupleStruct, generator: &Generator) -> String {
        generator
            .base()
            .join_subtypes(entity.subtypes.as_ref(), generator)
    }

    fn get_type_name(&self, entity: &TupleStruct, generator: &Generator) -> String {
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

    fn get_name(&self, entity: &TupleStruct, generator: &Generator) -> String {
        generator
            .base()
            .format_type_name(entity.name.as_str(), generator)
            .into()
    }

    fn macros(&self, _entity: &TupleStruct, _gen: &Generator) -> Cow<'static, str> {
        "#[derive(Default, Clone, PartialEq, Debug, UtilsTupleIo, UtilsDefaultSerde)]\n".into()
    }

    fn format_comment(&self, entity: &TupleStruct, generator: &Generator) -> String {
        generator
            .base()
            .format_comment(entity.comment.as_deref(), 0)
    }

    fn validation(&self, entity: &TupleStruct, generator: &Generator) -> Cow<'static, str> {
        let body = entity
            .facets
            .iter()
            .map(|f| {
                gen_facet_validation(&f.facet_type, "0", &self.get_type_name(entity, generator))
            })
            .fold(String::new(), |x, y| x + &y);
        Cow::Owned(gen_validate_impl(
            self.get_name(entity, generator).as_str(),
            body.as_str(),
        ))
    }
}

pub struct DefaultTupleStructGen;
impl TupleStructGenerator for DefaultTupleStructGen {}
