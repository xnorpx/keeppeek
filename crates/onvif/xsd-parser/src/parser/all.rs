use crate::parser::{
    node_parser::parse_node,
    types::{RsEntity, Struct, StructField, TypeModifier},
    utils::{enum_to_field, get_documentation, get_parent_name},
    xsd_elements::{ElementType, XsdNode},
};
use roxmltree::Node;
use std::cell::RefCell;

pub fn parse_all(node: &Node, parent: &Node) -> RsEntity {
    let name = get_parent_name(node);
    RsEntity::Struct(Struct {
        name: name.into(),
        comment: get_documentation(parent),
        subtypes: vec![],
        fields: RefCell::new(elements_to_fields(node, name)),
        ..Default::default()
    })
}

fn elements_to_fields(choice: &Node, parent_name: &str) -> Vec<StructField> {
    choice
        .children()
        .filter(|n| n.is_element() && n.xsd_type() != ElementType::Annotation)
        .map(|n| match parse_node(&n, choice) {
            RsEntity::StructField(mut sf) => {
                if sf.type_name.ends_with(parent_name) {
                    sf.type_modifiers.push(TypeModifier::Recursive);
                }
                sf
            }
            RsEntity::Enum(mut en) => {
                en.name = format!("{parent_name}Choice");
                enum_to_field(en)
            }
            _ => unreachable!("\nError: {:?}\n{:?}", n, parse_node(&n, choice)),
        })
        .collect()
}
