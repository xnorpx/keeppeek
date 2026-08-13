use crate::generator::{
    Generator,
    alias::{AliasGenerator, DefaultAliasGen},
    base::{BaseGenerator, DefaultBaseGenerator},
    r#enum::{DefaultEnumGen, EnumGenerator},
    enum_case::{DefaultEnumCaseGen, EnumCaseGenerator},
    import::{DefaultImportGen, ImportGenerator},
    r#struct::{DefaultStructGen, StructGenerator},
    struct_field::{DefaultStructFieldGen, StructFieldGenerator},
    tuple_struct::{DefaultTupleStructGen, TupleStructGenerator},
};

#[derive(Default)]
pub struct GeneratorBuilder<'input> {
    generator: Generator<'input>,
}

#[allow(dead_code)]
impl<'input> GeneratorBuilder<'input> {
    pub fn with_base_gen(mut self, base: Box<dyn BaseGenerator>) -> Self {
        self.generator.base = Some(base);
        self
    }

    pub fn with_tuple_struct_gen(mut self, tsg: Box<dyn TupleStructGenerator>) -> Self {
        self.generator.tuple_struct_gen = Some(tsg);
        self
    }

    pub fn with_struct_gen(mut self, sg: Box<dyn StructGenerator>) -> Self {
        self.generator.struct_gen = Some(sg);
        self
    }

    pub fn with_struct_field_gen(mut self, sfg: Box<dyn StructFieldGenerator>) -> Self {
        self.generator.struct_field_gen = Some(sfg);
        self
    }

    pub fn with_enum_case_gen(mut self, ecg: Box<dyn EnumCaseGenerator>) -> Self {
        self.generator.enum_case_gen = Some(ecg);
        self
    }

    pub fn with_enum_gen(mut self, eg: Box<dyn EnumGenerator>) -> Self {
        self.generator.enum_gen = Some(eg);
        self
    }

    pub fn with_alias_gen(mut self, al: Box<dyn AliasGenerator>) -> Self {
        self.generator.alias_gen = Some(al);
        self
    }

    pub fn with_import_gen(mut self, im: Box<dyn ImportGenerator>) -> Self {
        self.generator.import_gen = Some(im);
        self
    }

    pub fn build(self) -> Generator<'input> {
        let mut generator = self.generator;
        generator
            .base
            .get_or_insert_with(|| Box::new(DefaultBaseGenerator {})); //.set_target_ns(&generator.target_ns);

        generator
            .tuple_struct_gen
            .get_or_insert_with(|| Box::new(DefaultTupleStructGen {}));

        generator
            .struct_gen
            .get_or_insert_with(|| Box::new(DefaultStructGen {}));

        generator
            .struct_field_gen
            .get_or_insert_with(|| Box::new(DefaultStructFieldGen {}));

        generator
            .enum_case_gen
            .get_or_insert_with(|| Box::new(DefaultEnumCaseGen {}));

        generator
            .enum_gen
            .get_or_insert_with(|| Box::new(DefaultEnumGen {}));

        generator
            .alias_gen
            .get_or_insert_with(|| Box::new(DefaultAliasGen {}));

        generator
            .import_gen
            .get_or_insert_with(|| Box::new(DefaultImportGen {}));

        generator
    }
}

#[cfg(test)]
mod test {
    use crate::{
        generator::{Generator, builder::GeneratorBuilder, tuple_struct::TupleStructGenerator},
        parser::types::{RsEntity, TupleStruct},
    };

    fn test_generator_state(generator: &Generator) {
        assert!(generator.tuple_struct_gen.is_some());
        assert!(generator.struct_gen.is_some());
        assert!(generator.struct_field_gen.is_some());
        assert!(generator.base.is_some());
        assert!(generator.enum_case_gen.is_some());
        assert!(generator.enum_gen.is_some());
        assert!(generator.alias_gen.is_some());
        assert!(generator.import_gen.is_some());
    }

    #[test]
    fn test_builder_default() {
        let generator = GeneratorBuilder::default().build();
        test_generator_state(&generator);
        assert!(generator.target_ns.borrow().is_none());
    }

    #[test]
    fn test_builder_with_custom_generators() {
        struct StubTupleStructGen;
        impl TupleStructGenerator for StubTupleStructGen {
            fn generate(&self, _: &TupleStruct, _: &Generator) -> String {
                "Tuple struct".into()
            }
        }

        let generator = GeneratorBuilder::default()
            .with_tuple_struct_gen(Box::new(StubTupleStructGen {}))
            .build();

        test_generator_state(&generator);

        let ts = RsEntity::TupleStruct(TupleStruct::default());
        assert_eq!(generator.generate(&ts), "Tuple struct");
    }
}
