use std::collections::BTreeSet;

use crate::event::{
    ExpandedName, ItemList, Notification, PropertyOperation, ProtocolError, XmlElement,
};

const NAME_SIZE_BYTES_MAX: usize = 256;
const VALUE_SIZE_BYTES_MAX: usize = 4096;
const TEXT_SIZE_BYTES_MAX: usize = 64 * 1024;
const ATTRIBUTE_COUNT_MAX: usize = 32;

pub(super) fn validate(notification: &Notification) -> Result<(), ProtocolError> {
    let mut budget = Budget::default();
    budget.string(&notification.topic.dialect, VALUE_SIZE_BYTES_MAX)?;
    for name in &notification.topic.path {
        budget.name(name)?;
    }
    if let Some(PropertyOperation::Other(value)) = &notification.property_operation {
        budget.string(value, NAME_SIZE_BYTES_MAX)?;
    }
    for section in [&notification.source, &notification.key, &notification.data] {
        budget.section(section)?;
    }
    Ok(())
}

#[derive(Default)]
struct Budget {
    bytes: usize,
    items: usize,
    nodes: usize,
}

impl Budget {
    const fn string(&mut self, value: &str, limit: usize) -> Result<(), ProtocolError> {
        if value.len() > limit {
            return Err(ProtocolError("normalization string limit exceeded"));
        }
        self.bytes += value.len();
        if self.bytes > crate::event::NOTIFICATION_XML_SIZE_BYTES_MAX {
            return Err(ProtocolError("normalization byte limit exceeded"));
        }
        Ok(())
    }

    fn name(&mut self, name: &ExpandedName) -> Result<(), ProtocolError> {
        if name.local_name.is_empty() {
            return Err(ProtocolError("empty normalization XML name"));
        }
        self.string(&name.local_name, NAME_SIZE_BYTES_MAX)?;
        self.string(
            name.namespace_uri.as_deref().unwrap_or_default(),
            VALUE_SIZE_BYTES_MAX,
        )
    }

    fn section(&mut self, section: &ItemList) -> Result<(), ProtocolError> {
        for count in [section.simple.len(), section.element.len()] {
            if count > crate::event::ITEM_COUNT_MAX - self.items {
                return Err(ProtocolError("normalization item limit exceeded"));
            }
            self.items += count;
        }
        let mut names = BTreeSet::new();
        for name in section
            .simple
            .iter()
            .map(|item| &item.name)
            .chain(section.element.iter().map(|item| &item.name))
        {
            self.string(name, NAME_SIZE_BYTES_MAX)?;
            if name.is_empty() || !names.insert(name) {
                return Err(ProtocolError("duplicate or empty normalization item name"));
            }
        }
        for item in &section.simple {
            self.string(&item.value, VALUE_SIZE_BYTES_MAX)?;
        }
        for item in &section.element {
            self.element(&item.value)?;
        }
        Ok(())
    }

    fn element(&mut self, root: &XmlElement) -> Result<(), ProtocolError> {
        let mut pending = Vec::with_capacity(crate::event::ELEMENT_NODE_COUNT_MAX);
        pending.push((root, 1));
        while let Some((element, depth)) = pending.pop() {
            self.nodes += 1;
            if depth > crate::event::XML_DEPTH_MAX
                || self.nodes > crate::event::ELEMENT_NODE_COUNT_MAX
                || element.children.len() + pending.len()
                    > crate::event::ELEMENT_NODE_COUNT_MAX - self.nodes
                || element.attributes.len() > ATTRIBUTE_COUNT_MAX
            {
                return Err(ProtocolError("normalization XML limit exceeded"));
            }
            self.name(&element.name)?;
            self.string(&element.text, TEXT_SIZE_BYTES_MAX)?;
            let mut attributes = BTreeSet::new();
            for attribute in &element.attributes {
                self.name(&attribute.name)?;
                self.string(&attribute.value, VALUE_SIZE_BYTES_MAX)?;
                if !attributes.insert((
                    attribute.name.namespace_uri.as_deref().unwrap_or_default(),
                    &attribute.name.local_name,
                )) {
                    return Err(ProtocolError("duplicate normalization XML attribute"));
                }
            }
            pending.extend(element.children.iter().map(|child| (child, depth + 1)));
        }
        Ok(())
    }
}

pub(super) fn value<'a>(items: &'a ItemList, name: &str) -> Option<&'a str> {
    items
        .simple
        .iter()
        .find(|item| item.name == name)
        .map(|item| item.value.as_str())
}

pub(super) struct State {
    pub(super) value: Option<bool>,
    pub(super) unknown: bool,
}

pub(super) fn state(items: &ItemList, names: &[&str]) -> Result<State, ProtocolError> {
    let mut state = None;
    let mut unknown = false;
    for item in &items.simple {
        if !names.contains(&item.name.as_str()) {
            continue;
        }
        let Some(value) = boolean(&item.value) else {
            unknown = true;
            continue;
        };
        if state.is_some_and(|previous| previous != value) {
            return Err(ProtocolError("contradictory normalization states"));
        }
        state = Some(value);
    }
    Ok(State {
        value: state,
        unknown,
    })
}

pub(super) fn source(items: &ItemList) -> Option<String> {
    [
        "VideoSourceConfigurationToken",
        "VideoSourceToken",
        "VideoSource",
        "Source",
        "ChannelID",
        "ChannelId",
        "channelID",
        "Channel",
    ]
    .into_iter()
    .filter_map(|name| value(items, name))
    .find(|value| !value.trim().is_empty())
    .map(str::to_owned)
}

pub(super) fn rule(notification: &Notification) -> Result<Option<String>, ProtocolError> {
    let mut rule = None;
    for item in notification
        .source
        .simple
        .iter()
        .chain(&notification.key.simple)
    {
        if !["Rule", "RuleName", "RuleToken", "RuleId", "RuleID"].contains(&item.name.as_str())
            || item.value.trim().is_empty()
        {
            continue;
        }
        if rule.is_some_and(|previous| previous != item.value) {
            return Err(ProtocolError(
                "contradictory normalization rule identifiers",
            ));
        }
        rule = Some(item.value.as_str());
    }
    Ok(rule.map(str::to_owned))
}

fn boolean(value: &str) -> Option<bool> {
    let value = value.trim();
    if ["true", "1", "on", "active"]
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
    {
        Some(true)
    } else if ["false", "0", "off", "inactive"]
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
    {
        Some(false)
    } else {
        None
    }
}
