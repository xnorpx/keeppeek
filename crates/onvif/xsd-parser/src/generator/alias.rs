use crate::{generator::Generator, parser::types::Alias};

pub trait AliasGenerator {
    fn generate(&self, entity: &Alias, generator: &Generator) -> String {
        format!(
            "//{comment} pub type {name} = {original};\n",
            comment = self.format_comment(entity.comment.as_deref(), generator),
            name = self.format_name(entity.name.as_str(), generator),
            original = self.format_original_type(entity.original.as_str(), generator)
        )
    }

    fn format_comment(&self, comment: Option<&str>, generator: &Generator) -> String {
        generator.base().format_comment(comment, 0)
    }

    fn format_name(&self, name: &str, generator: &Generator) -> String {
        generator.base().format_type_name(name, generator).into()
    }

    fn format_original_type(&self, name: &str, generator: &Generator) -> String {
        generator.base().format_type_name(name, generator).into()
    }
}

pub struct DefaultAliasGen;
impl AliasGenerator for DefaultAliasGen {}
