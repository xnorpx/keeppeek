use xmltree::Element;

use super::{ProtocolError, SCHEMA, tree, xml};

pub(super) fn parse(appearance: &Element) -> Result<Option<String>, ProtocolError> {
    let plate = field(appearance, "LicensePlateInfo", "PlateNumber")?;
    let barcode = field(appearance, "BarcodeInfo", "Data")?;
    Ok(plate.or(barcode))
}

fn field(
    appearance: &Element,
    container: &str,
    name: &str,
) -> Result<Option<String>, ProtocolError> {
    let Some(container) = xml::child(appearance, SCHEMA, container)? else {
        return Ok(None);
    };
    let element = xml::required(container, SCHEMA, name)?;
    let _likelihood = tree::likelihood(element)?;
    Ok(Some(tree::scalar(element)?))
}
