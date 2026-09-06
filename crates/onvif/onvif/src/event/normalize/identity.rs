use crate::event::{ExpandedName, ItemList, Notification, ProtocolError, XmlElement};

const IDENTITY_SIZE_BYTES_MAX: usize = 4096;

pub(super) fn canonical(notification: &Notification) -> Result<String, ProtocolError> {
    let mut identity = Identity(String::with_capacity(IDENTITY_SIZE_BYTES_MAX));
    identity.part("onvif:1")?;
    identity.count(notification.topic.path.len())?;
    for name in &notification.topic.path {
        identity.part(name.namespace_uri.as_deref().unwrap_or(super::TOPICS))?;
        identity.part(&name.local_name)?;
    }
    identity.section(&notification.source)?;
    identity.section(&notification.key)?;
    identity.0.shrink_to_fit();
    Ok(identity.0)
}

struct Identity(String);

impl Identity {
    fn part(&mut self, value: &str) -> Result<(), ProtocolError> {
        let length = value.len().to_string();
        let available = IDENTITY_SIZE_BYTES_MAX - self.0.len();
        if value.len() > available || length.len() + 1 > available - value.len() {
            return Err(ProtocolError("normalization identity limit exceeded"));
        }
        self.0.push_str(&length);
        self.0.push(':');
        self.0.push_str(value);
        Ok(())
    }

    fn count(&mut self, count: usize) -> Result<(), ProtocolError> {
        self.part(&count.to_string())
    }

    fn name(&mut self, name: &ExpandedName) -> Result<(), ProtocolError> {
        self.part(name.namespace_uri.as_deref().unwrap_or_default())?;
        self.part(&name.local_name)
    }

    fn section(&mut self, section: &ItemList) -> Result<(), ProtocolError> {
        let mut simple: Vec<_> = section.simple.iter().collect();
        simple.sort_unstable_by_key(|item| &item.name);
        self.count(simple.len())?;
        for item in simple {
            self.part(&item.name)?;
            self.part(&item.value)?;
        }
        let mut elements: Vec<_> = section.element.iter().collect();
        elements.sort_unstable_by_key(|item| &item.name);
        self.count(elements.len())?;
        for item in elements {
            self.part(&item.name)?;
            self.element(&item.value)?;
        }
        Ok(())
    }

    fn element(&mut self, element: &XmlElement) -> Result<(), ProtocolError> {
        self.name(&element.name)?;
        let mut attributes: Vec<_> = element.attributes.iter().collect();
        attributes.sort_unstable_by_key(|attribute| {
            (
                attribute.name.namespace_uri.as_deref().unwrap_or_default(),
                &attribute.name.local_name,
            )
        });
        self.count(attributes.len())?;
        for attribute in attributes {
            self.name(&attribute.name)?;
            self.part(&attribute.value)?;
        }
        self.part(&element.text)?;
        self.count(element.children.len())?;
        for child in &element.children {
            self.element(child)?;
        }
        Ok(())
    }
}
