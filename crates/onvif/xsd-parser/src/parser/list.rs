use crate::parser::{
    constants::attribute,
    node_parser::parse_node,
    types::{RsEntity, TupleStruct, TypeModifier},
    utils::find_child,
};
use roxmltree::Node;

pub fn parse_list(list: &Node) -> RsEntity {
    let mut result = list.attribute(attribute::ITEM_TYPE).map_or_else(
        || {
            let nested_simple_type = find_child(list, "simpleType").expect(
                "itemType not allowed if the content contains a simpleType element. Otherwise, required."
            );

            match parse_node(&nested_simple_type, list) {
                RsEntity::Enum(en) => TupleStruct {
                    type_name: en.name.clone(),
                    subtypes: vec![RsEntity::Enum(en)],
                    ..Default::default()
                },
                RsEntity::TupleStruct(ts) => ts,
                _ => unreachable!(),
            }
        },
        |item_type| TupleStruct {
            type_name: item_type.to_string(),
            ..Default::default()
        },
    );
    result.type_modifiers.push(TypeModifier::Array);
    RsEntity::TupleStruct(result)
}
